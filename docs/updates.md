<!-- update-contract: v1 -->
# Updates

The `rusty-mirror` CLI tells you when a newer release exists and updates
itself with one command. Automatic installs stay off until you turn them on.

## The daily check

After a command finishes on a terminal, the CLI may start a small detached
helper. At most once a day it makes one anonymous GET of the public GitHub
release record of the repository named in the package metadata:

```text
https://api.github.com/repos/michael-berardi/rusty-mirror/releases/latest
```

The request carries the fixed User-Agent `rusty-mirror-update-check`. It sends
no version, identifier, cookie or machine data, and curl config files are
ignored. It times out after 4 seconds. A failure is recorded with its cause
and is not retried until the next day. Only stable `x.y.z` releases newer
than yours count. Drafts, prereleases and other tags are ignored.

There is no check and no notice when stdout or stderr is not a terminal, when
`CI` is set, in agent shells (`CLAUDECODE`, `CODEX_SANDBOX`, `GEMINI_CLI`),
with `RUSTY_MIRROR_NO_UPDATE_CHECK=1`, or after `rusty-mirror update --notify off`.

## The notice

The notice goes to stderr after your command, never into its JSON output:

```text
rusty-mirror 0.4.0 available (you have 0.3.0) · run: rusty-mirror update
turn on automatic updates: rusty-mirror update --auto on · skip: rusty-mirror update --skip
```

The second line is dim, and only offers `--skip` once automatic installs are
on. Each version is announced at most once a day. A skipped version stays
quiet until a newer one is released.

## Commands

| Command | Effect |
| --- | --- |
| `rusty-mirror update` | Check now, show the new version and its notes, ask `Install 0.4.0 now? [Y/n]`, install |
| `rusty-mirror update --check` | Report only: exit 0 up to date, 10 update available, 1 check failed (with the cause) |
| `rusty-mirror update --skip` | Hide the current release's notice |
| `rusty-mirror update --auto on\|off\|status` | Automatic installs (off by default) |
| `rusty-mirror update --notify on\|off\|status` | The daily check and notice (on by default) |
| `rusty-mirror update --rollback` | Reinstall the version that ran before the last update |

After a successful `update` on a terminal, the CLI asks once, `Install future
updates automatically? [y/N]`, and remembers the answer. Without a terminal,
`update` installs without asking, because running it is consent. Like every
command, `update` prints one JSON document on stdout.

With automatic installs on, the daily check installs a new release from the
detached helper at low priority. The new version runs from your next command.
Each version is tried once. If the install fails, your current version stays.
The cause goes into `update.log` and `update --auto status`, and the notice
comes back.

## How updates are installed

The updater detects how this binary was installed.

**One-line / installer installs.** `scripts/install.sh` puts the binary in
`~/.rusty-mirror/releases/<version>/`, points `~/.rusty-mirror/current` at that
folder and writes a marked shim in `~/.local/bin`. `update` downloads
`install.sh`, `SHA256SUMS` and this platform's archive for `v<version>` from

```text
https://github.com/michael-berardi/rusty-mirror/releases/download/v<version>/
```

into a private 0700 build folder, verifies both `install.sh` and the archive
against `SHA256SUMS` before running anything, then runs `sh install.sh` with
`RUSTY_MIRROR_RELEASE_BASE` set to that folder, plus `RUSTY_MIRROR_VERSION`,
`RUSTY_MIRROR_HOME` and `RUSTY_MIRROR_BIN_DIR`, and with stdin closed. The
installer performs its own checksum check and re-points `current`; a failed
download, checksum or install leaves the running version in place. The build
folder is removed on success, failure and interruption. For tests only,
`RUSTY_MIRROR_UPDATE_RELEASE_BASE` replaces the download URL with a local
release directory; release assets themselves are never unverified.

**Cargo installs.** If Cargo owns the binary (`.crates2.json` in the install
root), `update` runs the same command that installed it:

```sh
cargo install --git https://github.com/michael-berardi/rusty-mirror --tag v0.4.0 --locked --force rusty-mirror-cli
```

Cargo fetches the tag over HTTPS, checks every crate against the checksums in
the tag's `Cargo.lock`, and replaces the binary only after a successful build.
A local build with neither marker is left alone, and the reported `command`
tells you how to install one.

`update --rollback` restores the previous version. For an installer install it
only re-points `current` at the previous release folder, which must still
exist; for a Cargo install it rebuilds the previous tag. No network or build is
needed for the installer case.

## Local files

| File | Contents |
| --- | --- |
| `update.json` (0600) | Last check time and result, skipped version, last notice, your two switches, the previous version |
| `update.log` (0600) | Automatic installs only, at most 256 KiB |

They live in `~/Library/Caches/rusty-mirror/` on macOS, and in
`$XDG_CACHE_HOME/rusty-mirror/` (default `~/.cache/rusty-mirror/`) elsewhere.
For testing, `RUSTY_MIRROR_UPDATE_CACHE` moves the folder, and
`RUSTY_MIRROR_UPDATE_API` points the check at another release record.

`rusty-mirror update --auto off` deletes both files and the folder. The one
exception is `--notify off`: a small file stays to remember that choice. To
stop the CLI and clear its state:

```sh
rusty-mirror update --notify on   # only if you turned notices off
rusty-mirror update --auto off
```

Then remove the binary with `cargo uninstall rusty-mirror-cli` for a Cargo
install, or delete the marked shim and `~/.rusty-mirror` for an installer
install. Cargo and Scripts keep their own download caches; see
[PRIVACY.md](PRIVACY.md) for what is on disk.
