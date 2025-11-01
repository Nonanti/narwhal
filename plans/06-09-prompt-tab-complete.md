# Plan 06-09 — `:` prompt tab-completion

## Why

`:open <name>`, `:remove <name>`, `:forget <name>`, `:help <name>`,
`:export <format> <path>` all take arguments narwhal already knows
the universe for. Typing them out is tedious; `:` should
tab-complete just like every other prompt.

## Scope

When the user is editing the `:`-prompt buffer and the buffer
matches one of the patterns below, Tab completes the *last token*
against the relevant universe:

- `:open <prefix>`      → connection names from `ConnectionsFile`
- `:remove <prefix>`    → same
- `:forget <prefix>`    → same
- `:rm <prefix>`        → same (alias of remove)
- `:help <prefix>`      → `BUILTIN_COMMAND_NAMES` ∪ plugin command
                          names
- `:export <prefix>`    → ["csv", "json"]
- bare `:` (empty buf)  → no completion (would be too noisy)

If exactly one match, Tab inserts it. If multiple matches, Tab
inserts the longest common prefix and shows the candidates in
the status bar (or a small popup; status bar is cheaper and
sufficient).

## Constraints

- The `:`-prompt is a different state from the editor — it lives
  on `AppCore` directly (see `command_buffer` or similar).
  Completion is per-prompt and doesn't share state with the
  editor's completion popup.
- One commit, conventional, long-form.
- `clippy --all-targets -- -D warnings`, `fmt --check`, no
  `unwrap`/`expect` in production.

## Concrete steps

### Step 1: locate the prompt state

Inspect `core.rs` for the `:` prompt — it's likely a
`command_buffer: String` or similar plus a `prompt_open: bool`.
Adapt to whatever shape exists.

### Step 2: complete_prompt() helper

```rust
fn complete_prompt(&mut self) {
    let buf = self.command_buffer.clone();
    let trimmed = buf.trim_start_matches(':');
    let mut parts: Vec<&str> = trimmed.split_whitespace().collect();
    let head = parts.first().copied().unwrap_or("");

    // Identify which universe to complete from.
    let universe: Vec<String> = match head {
        "open" | "remove" | "rm" | "forget" => self.connections
            .connections.iter().map(|c| c.name.clone()).collect(),
        "help" => {
            let mut v: Vec<String> = crate::commands::BUILTIN_COMMAND_NAMES
                .iter().map(|s| s.to_string()).collect();
            v.extend(self.plugins.catalogue().into_iter()
                .map(|(_, c)| c.name));
            v
        }
        "export" => vec!["csv".into(), "json".into()],
        _ => return,
    };

    // The token being completed is the last whitespace-separated
    // word in the buffer; if the buffer ends with whitespace, we
    // are starting a fresh token (empty prefix).
    let prefix = if buf.ends_with(char::is_whitespace) {
        ""
    } else {
        parts.last().copied().unwrap_or("")
    };

    let matches: Vec<&str> = universe.iter()
        .filter(|name| name.to_lowercase().starts_with(&prefix.to_lowercase()))
        .map(String::as_str)
        .collect();

    match matches.as_slice() {
        [] => {
            self.status.message = format!("no completions for {prefix:?}");
        }
        [only] => {
            // Replace the in-flight token with the unique match.
            self.replace_prompt_token(only);
        }
        many => {
            // Insert the longest common prefix and list the rest
            // in the status bar.
            let lcp = longest_common_prefix(many);
            if lcp.len() > prefix.len() {
                self.replace_prompt_token(&lcp);
            }
            self.status.message = format!("{}", many.join(" "));
        }
    }
}
```

### Step 3: longest_common_prefix helper

Trivial — walk character by character across the slice and stop
at the first divergence.

### Step 4: route Tab in prompt state

The keypress handler that owns the `:`-prompt currently does
something like:

```rust
match key.code {
    CtKey::Enter => self.submit_prompt(),
    CtKey::Esc   => self.close_prompt(),
    CtKey::Char(c) => self.command_buffer.push(c),
    CtKey::Backspace => { self.command_buffer.pop(); }
    _ => {}
}
```

Add:

```rust
CtKey::Tab => self.complete_prompt(),
```

### Step 5: replace_prompt_token

```rust
fn replace_prompt_token(&mut self, replacement: &str) {
    let buf = &mut self.command_buffer;
    let mut chars: Vec<char> = buf.chars().collect();
    while matches!(chars.last(), Some(c) if !c.is_whitespace()) {
        chars.pop();
    }
    chars.extend(replacement.chars());
    *buf = chars.into_iter().collect();
}
```

(Adapt if the buffer is `String` vs `Vec<char>` etc.)

## Files

- `crates/narwhal-app/src/core.rs` (complete_prompt,
  replace_prompt_token, Tab route)
- `crates/narwhal-app/tests/prompt_completion.rs` (new)

## Tests

`tests/prompt_completion.rs`:

1. `open_unique_completes_inline`: connection named "smoke",
   buffer `:open sm`, Tab → buffer `:open smoke`.
2. `open_multiple_inserts_lcp`: connections "smoke", "smolder",
   buffer `:open sm`, Tab → buffer `:open smo`, status lists both.
3. `help_completes_builtin`: buffer `:help op`, Tab → `:help open`.
4. `help_completes_plugin`: load a plugin with command "rc",
   buffer `:help r`, Tab → among results.
5. `export_completes_format`: buffer `:export c`, Tab → `:export csv`.
6. `unknown_head_is_noop`: buffer `:zz a`, Tab → no change.
7. `bare_colon_no_completion`: empty buffer (`:`), Tab → no change.

Acceptance: test count rises by **7**.

## Acceptance

- `nix develop --command cargo fmt --all -- --check` clean.
- `nix develop --command cargo clippy --all-targets -- -D warnings` clean.
- `nix develop --command cargo test --all` reports +7 from
  baseline.
- Manual smoke: `:open sm<Tab>` completes to `:open smoke`.

## Commit message template

```
feat(prompt): tab-completion for :open, :help, :export and friends

The :-prompt accepts a handful of subcommands whose arguments
narwhal already knows the universe for, so typing the rest out
by hand is friction the user shouldn't have to absorb.

Tab inside the prompt now completes the last token against the
relevant universe:

- :open <pref>   :remove <pref>   :rm <pref>   :forget <pref>
                 connection names from ConnectionsFile
- :help <pref>   built-in command names ∪ plugin command names
- :export <pref> csv | json

Exactly one match → inserted inline. Multiple matches → longest
common prefix inserted, candidates listed in the status bar so
the user can keep typing without losing track.

Seven new tests cover unique/multiple/unknown-head/empty-buffer
branches across each universe.
```
