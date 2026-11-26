# Changelog

## 0.3.0 — 2026-11-26

- CLI: `rusty-mirror update` installs the newest release with
  `cargo install --git … --tag vX --locked` into the same install root.
  `update --rollback` reinstalls the previous version.
- CLI: a daily anonymous release check, with a two-line notice on stderr on a
  terminal only. It is silent in CI, agent shells, pipes and JSON output, and
  off with `RUSTY_MIRROR_NO_UPDATE_CHECK=1` or `update --notify off`.
- CLI: `update --check` (exit 10 when an update is available), `--skip`,
  `--auto on|off|status` (off by default) and `--notify on|off|status`.
- Docs: `docs/updates.md` and `PRIVACY.md`.

## 0.2.0 — 2026-10-31

- Neutralized personal/project names and provider-specific paths in README, SECURITY and integration docs.
- Added a neutrality integration test (`crates/rusty-mirror/tests/neutrality.rs`).

## 0.1.1 — 2026-10-16

- Publisher renamed to Pareto Cybernetics in the crate metadata and licence. No functional changes.

## 0.1.0 — 2026-09-25

First public release.

- `rusty-mirror` Tauri 2 plugin: hidden, unfocused, incognito mirror window
  seeded from the primary; native invoke allowlist (`guard`); mirror-only CSP,
  navigation lock, memory-only storage; hot-served staged revisions with
  refresh, rollback and a vetoable apply; finite lifetime.
- macOS native capture through `WKWebView.takeSnapshot`, with no window shown
  and no Screen Recording permission.
- `rusty-mirror` CLI: `doctor`, `state`, `open`, `eval`, `capture`, `resize`,
  `screen`, `stage`, `refresh`, `rollback`, `apply`, `close`, `run`, `audit`.
- `rusty-mirror-core`: paths, wire protocol v1, verified staging, audits.
- Tally example app and an unattended macOS end-to-end test.
- Agent skill in `skills/rusty-mirror`.
