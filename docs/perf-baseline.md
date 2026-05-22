# Performance Baseline (Phase 2, pre-optimisation)

Captured at tag `phase-2-start` with
`nix develop -c cargo bench --workspace -- --quick`
on the host's release profile (`opt-level=3`, `lto=thin`,
`codegen-units=1`).

`--quick` reduces criterion's sample size for fast iteration; the
absolute numbers below are within ±5% of the long-form run on the
same hardware.

| Bench                                  | Baseline   | Notes                              |
| -------------------------------------- | ---------- | ---------------------------------- |
| splitter/plain_200                     | 13.66 µs   | ~900 MiB/s — already excellent     |
| splitter/dollar_quoted_50              | 4.36 µs    | 1.30 GiB/s — memchr::memmem path   |
| splitter/with_line_comments_500        | 17.51 µs   | 778 MiB/s                          |
| splitter/one_huge_statement_100kb      | 111 µs     | 876 MiB/s                          |
| sort/int_10k                           | 427 µs     | numeric cmp                        |
| sort/string_10k                        | 853 µs     | lexicographic                      |
| **sort/json_2k**                       | **6.68 ms**| `to_string()` on every compare     |
| editor_motion/word_forward_x10/50      | 1.63 µs    |                                    |
| editor_motion/word_forward_x10/500     | 13.7 µs    |                                    |
| **editor_motion/word_forward_x10/5000**| **230 µs** | `entire_text()` joins per motion   |
| append/payload_kb=0                    | 3.5 µs     |                                    |
| append/payload_kb=1                    | 4.0 µs     |                                    |
| append/payload_kb=8                    | 9.5 µs     |                                    |

Optimisation targets (highest expected return first):

1. **sort/json_2k** — replace `to_string()` comparator with structural
   compare over `serde_json::Value`.
2. **editor_motion word_forward at 5k lines** — drop the `entire_text()`
   join; walk the existing `lines: Vec<String>` in place.
3. Marginal: regex hot path in `Journal::append`; allocation in
   `format_count`/`format_elapsed`; `last_used.save` re-serialises the
   whole TOML on every `:open`.
