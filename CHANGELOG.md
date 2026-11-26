# Changelog

## 0.3.0 — 2026-11-26

- Added a one-line installer with SHA-256 verification, a per-tool install
  folder and a `~/.local/bin` shim. It refuses to replace a `rusty-mirror` it
  did not install; version and release base (local directory or `http(s)`/
  `file://` URL) can be overridden by environment. Linux installs require
  x86_64.
- Added a release builder for a Developer ID signed, notarized macOS universal
  binary and a Linux x86_64 binary, plus SHA256SUMS.
- Added an installer integration test that runs the installer over a local
  release directory with a temporary HOME.
- Hardened local privacy: captures and Tally data files are created 0600, with
  missing parent directories created 0700; the unattended script uses
  `umask 077`.
- Added privacy documentation and regression tests that reject telemetry
  packages, third-party endpoints and a non-local default CSP.
- Added anonymous daily update notices and a `rusty-mirror update`
  command for both install kinds. Script installs download `install.sh`,
  `SHA256SUMS` and the platform archive into a private build folder, verify
  both files before running, and rollback by re-pointing `current`; Cargo
  installs rebuild the previous tag. The daily check can be disabled with
  `RUSTY_MIRROR_NO_UPDATE_CHECK=1`. See `docs/updates.md` and `PRIVACY.md`.

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
