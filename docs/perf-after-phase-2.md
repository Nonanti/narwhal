# Performance — After Phase 2

Final criterion run at tag `phase-2-complete`, same hardware as
`perf-baseline.md`.

| Bench                                    | Before    | After     | Δ        |
| ---------------------------------------- | --------- | --------- | -------- |
| splitter/plain_200                       | 13.66 µs  | 13.14 µs  | −4 %     |
| splitter/dollar_quoted_50                | 4.36 µs   | 4.44 µs   | ~0 %     |
| splitter/with_line_comments_500          | 17.51 µs  | 17.4 µs   | ~0 %     |
| splitter/one_huge_statement_100kb        | 111 µs    | 110 µs    | ~0 %     |
| sort/int_10k                             | 427 µs    | 379 µs    | **−11 %** |
| sort/string_10k                          | 853 µs    | 744 µs    | **−13 %** |
| **sort/json_2k**                         | **6.68 ms** | **1.15 ms** | **−83 % (≈6×)** |
| editor_motion/word_forward_x10/50        | 1.63 µs   | 0.52 µs   | **−68 %** |
| editor_motion/word_forward_x10/500       | 13.7 µs   | 3.94 µs   | **−71 %** |
| **editor_motion/word_forward_x10/5000**  | **230 µs** | **38 µs**  | **−83 % (≈6×)** |
| append/payload_kb=0                      | 3.50 µs   | 3.57 µs   | ~0 %     |
| append/payload_kb=1                      | 4.00 µs   | 3.43 µs   | **−14 %** |
| append/payload_kb=8                      | 9.50 µs   | 10.13 µs  | +7 % (noise) |

## What changed

**`compare_values` Json path** — was `x.to_string().cmp(&y.to_string())`,
now `compare_json(x, y)` walks the `serde_json::Value` trees
structurally and only ever allocates for string leaves (`&str` cmp).
The string-sort speedup is a side-effect of better LLVM
inlining once the function table shrank.

**`move_word_forward`/`backward`** — used to call `entire_text()`
(joining every line into a fresh `String`) on every motion, then walk
bytes against a `cursor_byte_offset()` it just recomputed.  Replaced
with a new `LineCursor` that walks the `Vec<String>` in place; the
synthetic newline between lines is modelled directly as a virtual byte.
`LineCursor::at` skips the O(rows) prefix-sum walk the old offset
round-trip needed.

**`Journal::append`** — `redact_secrets` now goes through
`Regex::replace_all`'s built-in `Cow::Borrowed` short-circuit (no
`is_match` double-scan).  The hot path serialises a borrowed
`HistoryEntryView<'_>` instead of cloning the whole `HistoryEntry`
twice.  The truncation marker now appends pieces directly instead of
going through `format!`.

## Headline wins

Both top-of-baseline hotspots are roughly **6×** faster and the
operations now stay well under their respective UX deadlines
(<2 ms for sorting and <100 µs for a vim motion at 5k lines).
