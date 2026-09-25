# Changelog

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
