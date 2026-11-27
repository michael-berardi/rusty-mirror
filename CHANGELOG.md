# Changelog

## 0.4.0 — 2026-11-26

- Install: a one-line installer for macOS 14+ (Developer ID signed, notarized
  universal binary) and Linux x86_64. It checks the archive against the
  release's `SHA256SUMS` before unpacking, installs into
  `~/.local/share/rusty-mirror/releases/<version>` with a `~/.local/bin` shim,
  never replaces a `rusty-mirror` it did not install, and keeps at most one
  earlier release. `RUSTY_MIRROR_INSTALL_DIR`, `RUSTY_MIRROR_BIN_DIR`,
  `RUSTY_MIRROR_VERSION` and `RUSTY_MIRROR_RELEASE_BASE` override it.
- Install: `scripts/build-release.sh` builds the release assets
  (`rusty-mirror-darwin-universal.tar.gz`, `rusty-mirror-linux-x86_64.tar.gz`,
  `install.sh`, `SHA256SUMS`).
- CLI: `rusty-mirror update` installs the newest release the way the CLI was
  installed. An installer copy downloads the archive, verifies its SHA-256,
  checks the version and swaps it in with one atomic link rename. A Cargo
  copy runs `cargo install --git … --tag vX --locked` into the same root.
  `update --rollback` goes back to the previous version; the replaced release
  folder is removed after the new one has run once, or after a week.
- CLI: a daily anonymous release check, with a two-line notice on stderr on a
  terminal only. It is silent in CI, agent shells, pipes and JSON output, and
  off with `RUSTY_MIRROR_NO_UPDATE_CHECK=1` or `update --notify off`.
- CLI: `update --check` (exit 10 when an update is available), `--skip`,
  `--auto on|off|status` (off by default) and `--notify on|off|status`.
- Privacy: captures create any missing parent directories 0700, in addition
  to their 0600 file mode; the Tally example writes its data file 0600 in a
  0700 directory; `scripts/e2e.sh` uses `umask 077`.
- Privacy: no third-party badges in the README, and a test that fails if
  telemetry packages, third-party endpoints or a non-local default CSP are
  reintroduced.
- Docs: `docs/updates.md`, `PRIVACY.md`, and a short README Privacy section.
- Tests: the installer and `update` run end to end over a local release
  folder with a temporary HOME; checksum mismatches are refused.

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
