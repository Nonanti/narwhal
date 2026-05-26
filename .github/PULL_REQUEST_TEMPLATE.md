<!--
Thanks for the PR. Fill in the sections below. Keep it short.
Linked issues auto-close on merge: use "Closes #123".
-->

## Summary

<!-- One paragraph: what changes and why. -->

## Related issue

Closes #

## Type of change

- [ ] `feat` — new functionality
- [ ] `fix` — bug fix
- [ ] `refactor` — internal change, no behaviour delta
- [ ] `perf` — performance improvement
- [ ] `docs` — documentation only
- [ ] `chore` — build / CI / tooling

## Checklist

- [ ] `cargo fmt --all` clean
- [ ] `cargo clippy --workspace --all-targets -- -D warnings` clean
- [ ] `cargo test --workspace` passes locally
- [ ] New behaviour has a test (or rationale below)
- [ ] `CHANGELOG.md` updated under `[Unreleased]`
- [ ] Public API changes documented with `///`
- [ ] No new `unwrap`/`expect`/`panic!`/`unreachable!` in prod code
- [ ] If a new crate was added: workspace deps wired, `#![forbid(unsafe_code)]` present

## Notes for reviewers

<!-- Anything tricky? Trade-offs? Follow-up issues? -->
