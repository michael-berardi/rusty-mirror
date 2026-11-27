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

When you run `rusty-mirror update`, an installer-installed CLI downloads
`install.sh`, `SHA256SUMS` and the platform archive from GitHub over HTTPS with
the same fixed User-Agent, verifies both files before running the installer,
and re-points its local `current` link. A Cargo-installed CLI runs `cargo
install` for the release tag; Cargo fetches the source from GitHub and crates
from crates.io. Automatic installs, if you turn them on, use the same paths.
Neither kind sends local files, versions or identifiers in a request.

## Local files

- `~/.rusty-mirror/<app-id>/control.sock` (0600): the same-user control socket.
- `~/.rusty-mirror/<app-id>/hot-assets/<sha256>/`: frontends you staged. Directories
  0700, files 0600, kept until you delete them.
- Captures at the path you pass to `capture`: files 0600. Missing parent
  directories are created 0700; existing folders are left alone. Nothing
  deletes captures automatically.
- The Tally example's counter in its app data directory or `$TALLY_DATA`: a
  0600 file in a 0700 directory. The unattended script uses `umask 077`.
- A script install keeps its binary in `~/.rusty-mirror/releases/<version>`,
  a `current` symlink, the installer marker and a marked shim in
  `~/.local/bin`; the archive is unpacked into a temporary 0700 folder. An
  update keeps only the new release folder and the one it replaced (for
  `update --rollback`) and removes older ones.
- Opening a mirror copies the primary's `localStorage`, `sessionStorage` and any
  state your snapshot hook returns into memory only. Rusty Mirror does not write
  that copy to disk; closing or expiry destroys it. A capture PNG can show
  whatever the app displays.

The daily check's result is kept in `update.json` (0600) in the platform cache
folder: `~/Library/Caches/rusty-mirror/` on macOS, `~/.cache/rusty-mirror/`
elsewhere. Automatic installs also write `update.log` (0600), capped at
256 KiB. `rusty-mirror update --auto off` deletes both files. Details are in
[docs/updates.md](docs/updates.md).

The mirror's default CSP limits webview traffic to the app's own origin and
Tauri IPC; it is not an offline sandbox, and a custom `.csp(...)` can widen it.
If your app talks to a service you configured, that provider receives what your
app sends; Rusty Mirror does not proxy, log or report it. The example UI loads
no remote fonts, scripts or CDNs. Installing from GitHub or adding the git
dependency downloads source code when you ask for it; there is no phone-home
afterwards.

To delete it all: close the mirror, then `rm -rf ~/.rusty-mirror`, remove your
capture files and, if you used Tally, its data file. Also remove the marked
shim from your bin directory and run `rusty-mirror update --auto off` to clear
`update.json` and `update.log` (run `rusty-mirror update --notify on` first if
you turned notices off).
