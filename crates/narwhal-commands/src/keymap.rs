//! Key chord representation, parsing, and the [`Keymap`] resolver.
//!
//! A *chord* is a single user-visible keystroke (e.g. `j`, `Ctrl+S`,
//! `Shift+Enter`). We model it as a normalised tuple of
//! [`crossterm::event::KeyCode`] + a normalised [`crossterm::event::KeyModifiers`]
//! mask so that case sensitivity, modifier ordering, and alias spelling all
//! collapse into a single canonical form before lookup.
//!
//! The [`Keymap`] is a flat `HashMap<(KeyGroup, KeyChord), Action>` populated
//! by [`Keymap::default`] (the built-in bindings) and optionally overridden
//! by TOML from the user's `config.toml`. Lookup is `O(1)`.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use thiserror::Error;

use crate::action::{Action, KeyGroup};

/// A normalised, hashable representation of a single keystroke.
///
/// Normalisation rules — invariants every constructor enforces:
///
/// - **Letters** are stored *lowercase* in [`KeyCode::Char`]. The `Shift`
///   modifier bit is set independently. So `K` parses as
///   `KeyChord { code: Char('k'), mods: SHIFT }` not `Char('K') | NONE`.
/// - **Symbols and digits** are stored as-is; we do not infer SHIFT from
///   characters like `!` or `?` because the producing key combo is
///   layout-dependent.
/// - Only the *meaningful* modifier bits (CTRL/ALT/SHIFT) are kept; SUPER,
///   HYPER, etc. are dropped because crossterm reports them inconsistently
///   across terminals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl KeyChord {
    /// Construct a chord, normalising the inputs.
    pub fn new(code: KeyCode, mods: KeyModifiers) -> Self {
        let mut mods = mods & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let code = match code {
            // Lowercase the letter and reflect uppercase into SHIFT.
            KeyCode::Char(c) if c.is_ascii_uppercase() => {
                mods |= KeyModifiers::SHIFT;
                KeyCode::Char(c.to_ascii_lowercase())
            }
            other => other,
        };
        Self { code, mods }
    }

    /// Build a chord from a crossterm key event (the host hands us one of these
    /// per keypress).
    pub fn from_event(event: KeyEvent) -> Self {
        Self::new(event.code, event.modifiers)
    }

    /// Plain ASCII letter shortcut: `KeyChord::ch('j')` → `j`.
    pub fn ch(c: char) -> Self {
        Self::new(KeyCode::Char(c), KeyModifiers::NONE)
    }

    /// `Ctrl+<letter>` shortcut.
    pub fn ctrl(c: char) -> Self {
        Self::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// `Shift+<letter>` shortcut. Pass the lowercase variant.
    pub fn shift(c: char) -> Self {
        Self::new(KeyCode::Char(c), KeyModifiers::SHIFT)
    }
}

/// Errors produced by [`KeyChord::parse`].
#[derive(Debug, Error, PartialEq, Eq)]
#[non_exhaustive]
pub enum ChordParseError {
    #[error("empty chord")]
    Empty,
    #[error("unknown modifier: '{0}'")]
    UnknownModifier(String),
    #[error("unknown key: '{0}'")]
    UnknownKey(String),
    #[error("duplicate modifier: '{0}'")]
    DuplicateModifier(String),
}

impl KeyChord {
    /// Parse a chord from a TOML string.
    ///
    /// Format: zero or more modifiers separated by `+`, then a key name.
    /// Modifiers are case-insensitive (`ctrl`, `CTRL`, `Ctrl` all work);
    /// key names follow the spellings under [`parse_key_name`].
    ///
    /// Examples that all succeed:
    /// - `"j"` → `Char('j')` no mods
    /// - `"K"` → `Char('k')` + SHIFT
    /// - `"ctrl+s"` → `Char('s')` + CTRL
    /// - `"shift+enter"` → `Enter` + SHIFT
    /// - `"alt+f4"` → `F(4)` + ALT
    ///
    /// Whitespace around tokens is ignored.
    pub fn parse(s: &str) -> Result<Self, ChordParseError> {
        let trimmed = s.trim();
        if trimmed.is_empty() {
            return Err(ChordParseError::Empty);
        }
        let parts: Vec<&str> = trimmed.split('+').map(str::trim).collect();
        let (key_name, modifier_names) = parts
            .split_last()
            .ok_or(ChordParseError::Empty)?;
        let mut mods = KeyModifiers::NONE;
        for raw in modifier_names {
            let bit = match raw.to_ascii_lowercase().as_str() {
                "ctrl" | "control" | "c" => KeyModifiers::CONTROL,
                "shift" | "s" => KeyModifiers::SHIFT,
                "alt" | "meta" | "a" | "m" => KeyModifiers::ALT,
                "" => return Err(ChordParseError::Empty),
                other => return Err(ChordParseError::UnknownModifier(other.to_owned())),
            };
            if mods.contains(bit) {
                return Err(ChordParseError::DuplicateModifier((*raw).to_owned()));
            }
            mods |= bit;
        }
        // Letter-case convention: a *bare* uppercase letter (`K`) means
        // SHIFT+k. A letter that already carries explicit modifiers
        // (`ctrl+S`) is treated as case-insensitive — the user almost
        // certainly meant Ctrl+S, not Ctrl+Shift+S.
        let normalised_key: String;
        let key_name_lookup: &str = if !modifier_names.is_empty() && key_name.chars().count() == 1
        {
            let ch = key_name.chars().next().unwrap_or(' ');
            if ch.is_ascii_uppercase() {
                normalised_key = ch.to_ascii_lowercase().to_string();
                normalised_key.as_str()
            } else {
                key_name
            }
        } else {
            key_name
        };
        let code = parse_key_name(key_name_lookup)?;
        Ok(Self::new(code, mods))
    }

    /// Format a chord back into the canonical TOML form. Round-trip with
    /// [`Self::parse`].
    pub fn to_string_canonical(self) -> String {
        let mut out = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            out.push_str("ctrl+");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            out.push_str("alt+");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            out.push_str("shift+");
        }
        out.push_str(&key_name(self.code));
        out
    }
}

fn parse_key_name(name: &str) -> Result<KeyCode, ChordParseError> {
    let lower = name.to_ascii_lowercase();
    let code = match lower.as_str() {
        "" => return Err(ChordParseError::Empty),
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "backspace" | "bs" => KeyCode::Backspace,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" | "pgup" => KeyCode::PageUp,
        "pagedown" | "pgdn" => KeyCode::PageDown,
        "insert" | "ins" => KeyCode::Insert,
        "delete" | "del" => KeyCode::Delete,
        // Function keys: f1..f24
        s if s.starts_with('f') => {
            if let Ok(n) = s[1..].parse::<u8>() {
                if (1..=24).contains(&n) {
                    KeyCode::F(n)
                } else {
                    return Err(ChordParseError::UnknownKey(name.to_owned()));
                }
            } else if name.chars().count() == 1 {
                // single-char 'f' / 'F' is the literal letter
                KeyCode::Char(name.chars().next().unwrap())
            } else {
                return Err(ChordParseError::UnknownKey(name.to_owned()));
            }
        }
        // Single visible char (use the *original* casing for symbols; SHIFT
        // is inferred by KeyChord::new for letters).
        _ if name.chars().count() == 1 => KeyCode::Char(name.chars().next().unwrap()),
        _ => return Err(ChordParseError::UnknownKey(name.to_owned())),
    };
    Ok(code)
}

fn key_name(code: KeyCode) -> String {
    match code {
        KeyCode::Enter => "enter".into(),
        KeyCode::Tab => "tab".into(),
        KeyCode::BackTab => "backtab".into(),
        KeyCode::Backspace => "backspace".into(),
        KeyCode::Esc => "esc".into(),
        KeyCode::Char(' ') => "space".into(),
        KeyCode::Left => "left".into(),
        KeyCode::Right => "right".into(),
        KeyCode::Up => "up".into(),
        KeyCode::Down => "down".into(),
        KeyCode::Home => "home".into(),
        KeyCode::End => "end".into(),
        KeyCode::PageUp => "pageup".into(),
        KeyCode::PageDown => "pagedown".into(),
        KeyCode::Insert => "insert".into(),
        KeyCode::Delete => "delete".into(),
        KeyCode::F(n) => format!("f{n}"),
        KeyCode::Char(c) => c.to_string(),
        other => format!("{other:?}").to_lowercase(),
    }
}

/// Resolved keymap: maps `(group, chord) → action`.
///
/// Populated by [`Keymap::default`] then overlaid with user TOML via
/// [`Keymap::apply_override`].
#[derive(Debug, Clone, Default)]
pub struct Keymap {
    bindings: HashMap<(KeyGroup, KeyChord), Action>,
}

impl Keymap {
    /// Build the keymap that ships out of the box.
    pub fn builtin() -> Self {
        let mut map = Self::default();
        map.install_defaults();
        map
    }

    /// Resolve the action bound to `chord` in `group`. `None` means the chord
    /// is unbound in that group; the caller may fall back to legacy hard-coded
    /// handling (filter prompt typing, etc.) or simply consume the key.
    pub fn resolve(&self, group: KeyGroup, chord: KeyChord) -> Option<Action> {
        self.bindings.get(&(group, chord)).copied()
    }

    /// Bind `chord` in `group` to `action`. Returns the previous binding, if
    /// any — useful for user-facing override diagnostics.
    pub fn bind(
        &mut self,
        group: KeyGroup,
        chord: KeyChord,
        action: Action,
    ) -> Option<Action> {
        self.bindings.insert((group, chord), action)
    }

    /// Remove a binding. Returns the previous action, if any.
    pub fn unbind(&mut self, group: KeyGroup, chord: KeyChord) -> Option<Action> {
        self.bindings.remove(&(group, chord))
    }

    /// Apply a parsed TOML override fragment on top of the existing bindings.
    /// User entries win over built-ins; setting an action to `"unbind"` removes
    /// the binding entirely.
    ///
    /// Returns the collected per-binding diagnostics; an empty vector means
    /// every entry was applied.
    pub fn apply_overrides(
        &mut self,
        overrides: &HashMap<KeyGroup, HashMap<String, String>>,
    ) -> Vec<KeymapOverrideError> {
        let mut diags = Vec::new();
        for (group, table) in overrides {
            for (chord_str, action_str) in table {
                let chord = match KeyChord::parse(chord_str) {
                    Ok(c) => c,
                    Err(e) => {
                        diags.push(KeymapOverrideError::Chord {
                            group: *group,
                            input: chord_str.clone(),
                            source: e,
                        });
                        continue;
                    }
                };
                let trimmed = action_str.trim();
                if trimmed.eq_ignore_ascii_case("unbind") || trimmed.is_empty() {
                    self.unbind(*group, chord);
                    continue;
                }
                // Deserialize the action via serde so the kebab-case spelling
                // is enforced in one place.
                let quoted = format!("\"{trimmed}\"");
                let action: Action = match serde_json::from_str(&quoted) {
                    Ok(a) => a,
                    Err(e) => {
                        diags.push(KeymapOverrideError::Action {
                            group: *group,
                            chord: chord_str.clone(),
                            input: action_str.clone(),
                            message: e.to_string(),
                        });
                        continue;
                    }
                };
                self.bind(*group, chord, action);
            }
        }
        diags
    }

    fn install_defaults(&mut self) {
        use Action as A;
        use KeyGroup::Results;

        // ─── Results pane: navigation ──────────────────────────────────
        self.bind(Results, KeyChord::ch('j'), A::ResultsMoveDown);
        self.bind(Results, KeyChord::new(KeyCode::Down, KeyModifiers::NONE), A::ResultsMoveDown);
        self.bind(Results, KeyChord::ch('k'), A::ResultsMoveUp);
        self.bind(Results, KeyChord::new(KeyCode::Up, KeyModifiers::NONE), A::ResultsMoveUp);
        self.bind(Results, KeyChord::ch('h'), A::ResultsMoveLeft);
        self.bind(Results, KeyChord::new(KeyCode::Left, KeyModifiers::NONE), A::ResultsMoveLeft);
        self.bind(Results, KeyChord::ch('l'), A::ResultsMoveRight);
        self.bind(Results, KeyChord::new(KeyCode::Right, KeyModifiers::NONE), A::ResultsMoveRight);
        self.bind(Results, KeyChord::ch('g'), A::ResultsFirstRow);
        self.bind(Results, KeyChord::shift('g'), A::ResultsLastRow);

        // ─── Sort / filter / search ────────────────────────────────────
        self.bind(Results, KeyChord::ch('s'), A::ResultsToggleSort);
        self.bind(Results, KeyChord::ch('/'), A::ResultsOpenFilterPrompt);
        self.bind(Results, KeyChord::ch('n'), A::ResultsNextMatch);
        self.bind(Results, KeyChord::shift('n'), A::ResultsPrevMatch);
        self.bind(Results, KeyChord::new(KeyCode::Esc, KeyModifiers::NONE), A::ResultsEscape);

        // ─── Per-cell / per-row ────────────────────────────────────────
        self.bind(Results, KeyChord::new(KeyCode::Enter, KeyModifiers::NONE), A::ResultsOpenCellPopup);
        self.bind(Results, KeyChord::new(KeyCode::Enter, KeyModifiers::SHIFT), A::ResultsOpenRowDetail);
        self.bind(Results, KeyChord::shift('r'), A::ResultsOpenRowDetail);
        self.bind(Results, KeyChord::ch('e'), A::ResultsStartCellEdit);
        self.bind(Results, KeyChord::ch('y'), A::ResultsYankCell);
        self.bind(Results, KeyChord::shift('y'), A::ResultsYankRow);

        // ─── Multi-statement leader (']r' / '[r' completion is host-side) ──
        self.bind(Results, KeyChord::ch(']'), A::ResultsNextStatementLeader);
        self.bind(Results, KeyChord::ch('['), A::ResultsPrevStatementLeader);

        // ─── Row CRUD + Pending changes ────────────────────────────────
        self.bind(Results, KeyChord::ch('o'), A::ResultsAppendRow);
        self.bind(Results, KeyChord::shift('o'), A::ResultsDuplicateRow);
        self.bind(Results, KeyChord::ch('d'), A::ResultsDeleteRow);
        self.bind(Results, KeyChord::ctrl('s'), A::ResultsCommitPending);
        self.bind(Results, KeyChord::ctrl('x'), A::ResultsDiscardPending);
        self.bind(Results, KeyChord::ctrl('p'), A::ResultsOpenPendingPreview);

        // ─── Metadata tabs ─────────────────────────────────────────────
        self.bind(Results, KeyChord::ch('1'), A::MetaTabRecords);
        self.bind(Results, KeyChord::ch('2'), A::MetaTabColumns);
        self.bind(Results, KeyChord::ch('3'), A::MetaTabConstraints);
        self.bind(Results, KeyChord::ch('4'), A::MetaTabForeignKeys);
        self.bind(Results, KeyChord::ch('5'), A::MetaTabIndexes);

        // ─── JSON viewer ───────────────────────────────────────────────
        self.bind(Results, KeyChord::ch('z'), A::OpenJsonViewerCell);
        self.bind(KeyGroup::RowDetail, KeyChord::shift('z'), A::OpenJsonViewerRow);
    }
}

/// Per-binding diagnostic produced by [`Keymap::apply_overrides`].
#[derive(Debug)]
pub enum KeymapOverrideError {
    Chord {
        group: KeyGroup,
        input: String,
        source: ChordParseError,
    },
    Action {
        group: KeyGroup,
        chord: String,
        input: String,
        message: String,
    },
}

impl std::fmt::Display for KeymapOverrideError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Chord { group, input, source } => write!(
                f,
                "[keymap.{}] '{input}': {source}",
                group.as_str()
            ),
            Self::Action {
                group,
                chord,
                input,
                message,
            } => write!(
                f,
                "[keymap.{}] '{chord}' = '{input}': {message}",
                group.as_str()
            ),
        }
    }
}

impl std::error::Error for KeymapOverrideError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_letter_is_lowercase() {
        let chord = KeyChord::parse("j").unwrap();
        assert_eq!(chord.code, KeyCode::Char('j'));
        assert_eq!(chord.mods, KeyModifiers::NONE);
    }

    #[test]
    fn parse_uppercase_letter_implies_shift() {
        let chord = KeyChord::parse("G").unwrap();
        assert_eq!(chord.code, KeyCode::Char('g'));
        assert_eq!(chord.mods, KeyModifiers::SHIFT);
    }

    #[test]
    fn parse_ctrl_modifier() {
        let chord = KeyChord::parse("ctrl+s").unwrap();
        assert_eq!(chord.code, KeyCode::Char('s'));
        assert_eq!(chord.mods, KeyModifiers::CONTROL);
    }

    #[test]
    fn parse_shift_enter() {
        let chord = KeyChord::parse("shift+enter").unwrap();
        assert_eq!(chord.code, KeyCode::Enter);
        assert_eq!(chord.mods, KeyModifiers::SHIFT);
    }

    #[test]
    fn parse_function_key() {
        let chord = KeyChord::parse("F4").unwrap();
        assert_eq!(chord.code, KeyCode::F(4));
        let chord = KeyChord::parse("alt+f4").unwrap();
        assert_eq!(chord.code, KeyCode::F(4));
        assert_eq!(chord.mods, KeyModifiers::ALT);
    }

    #[test]
    fn parse_symbols_unchanged() {
        let chord = KeyChord::parse("/").unwrap();
        assert_eq!(chord.code, KeyCode::Char('/'));
        assert_eq!(chord.mods, KeyModifiers::NONE);
    }

    #[test]
    fn parse_space_keyword() {
        let chord = KeyChord::parse("ctrl+space").unwrap();
        assert_eq!(chord.code, KeyCode::Char(' '));
        assert_eq!(chord.mods, KeyModifiers::CONTROL);
    }

    #[test]
    fn parse_modifier_case_insensitive() {
        let a = KeyChord::parse("CTRL+S").unwrap();
        let b = KeyChord::parse("ctrl+s").unwrap();
        let c = KeyChord::parse("Ctrl+s").unwrap();
        assert_eq!(a, b);
        assert_eq!(b, c);
    }

    #[test]
    fn parse_duplicate_modifier_rejected() {
        let err = KeyChord::parse("ctrl+ctrl+s").unwrap_err();
        assert!(matches!(err, ChordParseError::DuplicateModifier(_)));
    }

    #[test]
    fn parse_unknown_modifier_rejected() {
        let err = KeyChord::parse("hyper+s").unwrap_err();
        assert!(matches!(err, ChordParseError::UnknownModifier(_)));
    }

    #[test]
    fn parse_unknown_key_rejected() {
        let err = KeyChord::parse("foobar").unwrap_err();
        assert!(matches!(err, ChordParseError::UnknownKey(_)));
    }

    #[test]
    fn canonical_string_roundtrips() {
        for raw in ["j", "G", "ctrl+s", "alt+f4", "shift+enter", "/", "ctrl+space"] {
            let chord = KeyChord::parse(raw).unwrap();
            let canon = chord.to_string_canonical();
            let again = KeyChord::parse(&canon).unwrap();
            assert_eq!(chord, again, "raw={raw} canon={canon}");
        }
    }

    #[test]
    fn from_event_normalises_uppercase() {
        let ev = KeyEvent::new(KeyCode::Char('K'), KeyModifiers::NONE);
        let chord = KeyChord::from_event(ev);
        assert_eq!(chord, KeyChord::shift('k'));
    }

    #[test]
    fn builtin_resolves_j_in_results() {
        let map = Keymap::builtin();
        let chord = KeyChord::ch('j');
        assert_eq!(
            map.resolve(KeyGroup::Results, chord),
            Some(Action::ResultsMoveDown)
        );
    }

    #[test]
    fn builtin_resolves_ctrl_s_to_commit_pending() {
        let map = Keymap::builtin();
        assert_eq!(
            map.resolve(KeyGroup::Results, KeyChord::ctrl('s')),
            Some(Action::ResultsCommitPending)
        );
    }

    #[test]
    fn builtin_does_not_bind_random_chord() {
        let map = Keymap::builtin();
        assert_eq!(
            map.resolve(KeyGroup::Results, KeyChord::ctrl('q')),
            None
        );
    }

    #[test]
    fn override_replaces_builtin_binding() {
        let mut map = Keymap::builtin();
        let mut group: HashMap<String, String> = HashMap::new();
        group.insert("ctrl+s".into(), "results-discard-pending".into());
        let mut all = HashMap::new();
        all.insert(KeyGroup::Results, group);
        let diags = map.apply_overrides(&all);
        assert!(diags.is_empty(), "{diags:?}");
        assert_eq!(
            map.resolve(KeyGroup::Results, KeyChord::ctrl('s')),
            Some(Action::ResultsDiscardPending)
        );
    }

    #[test]
    fn override_unbind_removes_binding() {
        let mut map = Keymap::builtin();
        let mut group: HashMap<String, String> = HashMap::new();
        group.insert("ctrl+s".into(), "unbind".into());
        let mut all = HashMap::new();
        all.insert(KeyGroup::Results, group);
        let diags = map.apply_overrides(&all);
        assert!(diags.is_empty());
        assert_eq!(
            map.resolve(KeyGroup::Results, KeyChord::ctrl('s')),
            None
        );
    }

    #[test]
    fn override_invalid_chord_reports_diagnostic() {
        let mut map = Keymap::builtin();
        let mut group: HashMap<String, String> = HashMap::new();
        group.insert("hyper+oof".into(), "results-move-down".into());
        let mut all = HashMap::new();
        all.insert(KeyGroup::Results, group);
        let diags = map.apply_overrides(&all);
        assert_eq!(diags.len(), 1);
    }

    #[test]
    fn override_invalid_action_reports_diagnostic() {
        let mut map = Keymap::builtin();
        let mut group: HashMap<String, String> = HashMap::new();
        group.insert("ctrl+q".into(), "results-quack".into());
        let mut all = HashMap::new();
        all.insert(KeyGroup::Results, group);
        let diags = map.apply_overrides(&all);
        assert_eq!(diags.len(), 1);
    }
}
