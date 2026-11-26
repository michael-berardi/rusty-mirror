# Changelog

## 0.2.1 — 2026-11-26

Privacy hardening: captures create any missing parent directories 0700, in
addition to their 0600 file mode; the Tally example writes its data
file 0600 in a 0700 directory; the unattended script uses `umask 077`.

Added a README Privacy section, removed third-party release badges from the
README, and added a test that fails if telemetry packages, third-party
endpoints or a non-local default CSP are reintroduced. No telemetry or update
checks were present; nothing was removed from user configuration.

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
