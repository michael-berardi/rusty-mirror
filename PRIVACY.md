# Privacy

Rusty Mirror has no telemetry, analytics, crash reporting or machine
identifier. Mirrors, captures and staged frontends stay on your machine. The
example UI loads no remote fonts, scripts or CDNs. See the
[security model](docs/security.md).

## Network

The CLI makes one network request on its own: the daily update check.

- One anonymous GET of this repository's public GitHub release record,
  `https://api.github.com/repos/michael-berardi/rusty-mirror/releases/latest`.
- Fixed User-Agent `rusty-mirror-update-check`. No version, identifier,
  cookie, token or machine data, and curl config files are ignored. As with
  any web request, GitHub sees your IP address.
- At most once a day, from a detached helper, with a 4-second timeout.
- Never in CI, in agent shells or without a terminal. To turn it off, set
  `RUSTY_MIRROR_NO_UPDATE_CHECK=1` or run `rusty-mirror update --notify off`.

`rusty-mirror update`, and automatic installs if you turn them on, download
the new release the way you installed it: for an installer copy, the release
archive and `SHA256SUMS` from the GitHub release, with the same fixed
User-Agent; for a Cargo copy, `cargo install` fetches the tag's source from
GitHub and its dependencies from crates.io. The installer and the git
dependency download only when you run them.

The mirror's default CSP limits webview traffic to the app's own origin and
Tauri IPC. It is not an offline sandbox, and a custom `.csp(...)` can widen
it. If your app talks to a service you configured, that provider receives what
your app sends; Rusty Mirror does not proxy, log or report it.

## Local files

- `~/.rusty-mirror/<app-id>/control.sock` (0600, or under
  `$RUSTY_MIRROR_HOME`): the same-user control socket.
- `~/.rusty-mirror/<app-id>/hot-assets/<sha256>/`: frontends you staged.
  Directories 0700, files 0600, kept until you delete them.
- Captures at the path you pass to `capture`: files 0600; missing parent
  directories are created 0700. Nothing deletes captures automatically.
- The Tally example's counter in its app data directory or `$TALLY_DATA`: a
  0600 file in a 0700 directory.
- The installer copy in `~/.local/share/rusty-mirror/` (0700, or
  `$RUSTY_MIRROR_INSTALL_DIR`; a copy installed by 0.3.0 stays in
  `~/.rusty-mirror/`): the release in use and at most one earlier release for
  `update --rollback`, plus the shim `~/.local/bin/rusty-mirror`.
- The update check's result in `update.json` (0600) in the platform cache
  folder: `~/Library/Caches/rusty-mirror/` on macOS, `~/.cache/rusty-mirror/`
  elsewhere. Automatic installs also write `update.log` there, capped at
  256 KiB. `rusty-mirror update --auto off` deletes both files. Details are in
  [docs/updates.md](docs/updates.md).

Opening a mirror copies the primary's `localStorage`, `sessionStorage` and any
state your snapshot hook returns into memory only. Rusty Mirror does not write
that copy to disk; closing or expiry destroys it. A capture PNG can show
whatever the app displays.

## Deleting everything

Close the mirror, then `rm -rf ~/.rusty-mirror`, remove your capture files
and, if you used Tally, its data file. Run `rusty-mirror update --auto off` to
delete the update cache. To remove an installer copy, delete
`~/.local/share/rusty-mirror` and `~/.local/bin/rusty-mirror`.
