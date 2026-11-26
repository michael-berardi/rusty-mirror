# Privacy

Rusty Mirror has no telemetry, analytics, crash reporting or machine
identifier. Mirrors, captures and staged frontends stay on your machine. The
control socket and staged revisions live under `~/.rusty-mirror/` (or
`$RUSTY_MIRROR_HOME`), and captures go where you ask. See the
[security model](docs/security.md).

## Update check

The CLI makes one network request on its own: the daily update check.

- One anonymous GET of this repository's public GitHub release record,
  `https://api.github.com/repos/michael-berardi/rusty-mirror/releases/latest`.
- Fixed User-Agent `rusty-mirror-update-check`. No version, identifier,
  cookie, token or machine data, and curl config files are ignored. As with
  any web request, GitHub sees your IP address.
- At most once a day, from a detached helper, with a 4-second timeout.
- Never in CI, in agent shells or without a terminal. To turn it off, set
  `RUSTY_MIRROR_NO_UPDATE_CHECK=1` or run `rusty-mirror update --notify off`.

`rusty-mirror update`, and automatic installs if you turn them on, run
`cargo install`. Cargo downloads the release's source from GitHub and its
dependencies from crates.io.

## Local files

The check's result is kept in `update.json` (0600) in the platform cache
folder: `~/Library/Caches/rusty-mirror/` on macOS, `~/.cache/rusty-mirror/`
elsewhere. Automatic installs also write `update.log` there, capped at
256 KiB. `rusty-mirror update --auto off` deletes both files. Details are in
[docs/updates.md](docs/updates.md).
