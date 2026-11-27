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
rusty-mirror 0.5.0 available (you have 0.4.0) · run: rusty-mirror update
turn on automatic updates: rusty-mirror update --auto on · skip: rusty-mirror update --skip
```

The second line is dim, and only offers `--skip` once automatic installs are
on. Each version is announced at most once a day. A skipped version stays
quiet until a newer one is released.

## Commands

| Command | Effect |
| --- | --- |
| `rusty-mirror update` | Check now, show the new version and its notes, ask `Install 0.5.0 now? [Y/n]`, install |
| `rusty-mirror update --check` | Report only: exit 0 up to date, 10 update available, 1 check failed (with the cause) |
| `rusty-mirror update --skip` | Hide the current release's notice |
| `rusty-mirror update --auto on\|off\|status` | Automatic installs (off by default) |
| `rusty-mirror update --notify on\|off\|status` | The daily check and notice (on by default) |
| `rusty-mirror update --rollback` | Go back to the version that ran before the last update |

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

`update` installs the new release the same way you installed the CLI.

**With the installer** (`curl … install.sh | sh`), the CLI lives in
`~/.local/share/rusty-mirror/releases/<version>/` (a copy installed by 0.3.0
stays in `~/.rusty-mirror/releases/`), and the `current` link there points at
the version in use. `update`:

1. downloads the release's `SHA256SUMS` and this machine's archive
   (`rusty-mirror-darwin-universal.tar.gz` or
   `rusty-mirror-linux-x86_64.tar.gz`) from the GitHub release, over HTTPS,
   into a private folder inside the install folder;
2. refuses the archive unless its SHA-256 matches `SHA256SUMS`, before
   anything is unpacked;
3. unpacks it and checks that it is the expected version and runs here;
4. moves it to `releases/<new>/` and switches `current` to it with one atomic
   rename. The shim in `~/.local/bin` does not change.

Any failure leaves your current version in place, and the download folder is
deleted on success and on failure. The release you updated from is kept for
`update --rollback`, which switches back to it without a download. Once the
new version has run a command successfully, or a week after the switch, the
old release folder is removed. If it is already gone, `update --rollback`
downloads and verifies that release the same way. Never more than one earlier
release is kept, and running the installer again follows the same rule.

**With Cargo** (`cargo install --git … --tag vX --locked`), Cargo owns the
binary. `update` runs the same command for the new tag, into the same install
root:

```sh
cargo install --git https://github.com/michael-berardi/rusty-mirror --tag v0.5.0 --locked --force rusty-mirror-cli
```

Cargo fetches the tag over HTTPS. It builds exactly the dependencies in the
tag's `Cargo.lock`, checking each crate against the checksum recorded there.
It replaces the binary only after a successful build, so a failed fetch,
checksum or build leaves your current version in place. The build runs in a
private temporary folder that is deleted when it finishes, on success or failure.
If a build is killed, the next update removes its folder. Cargo keeps no copy
of the old binary, so `update --rollback` rebuilds the previous tag the same
way.

If this binary came from neither (a local build, for example), `update` leaves
it alone and prints the command to run instead.

## Local files

| File | Contents |
| --- | --- |
| `update.json` (0600) | Last check time and result, skipped version, last notice, your two switches, the previous version |
| `update.log` (0600) | Automatic installs only, at most 256 KiB |

They live in `~/Library/Caches/rusty-mirror/` on macOS, and in
`$XDG_CACHE_HOME/rusty-mirror/` (default `~/.cache/rusty-mirror/`) elsewhere.
For testing, `RUSTY_MIRROR_UPDATE_CACHE` moves the folder,
`RUSTY_MIRROR_UPDATE_API` points the check at another release record, and
`RUSTY_MIRROR_RELEASE_BASE` (a local folder or URL) replaces the GitHub
release downloads.

`rusty-mirror update --auto off` deletes both files and the folder. The one
exception is `--notify off`: a small file stays to remember that choice. To
uninstall completely:

```sh
rusty-mirror update --notify on   # only if you turned notices off
rusty-mirror update --auto off
rm -rf ~/.local/share/rusty-mirror ~/.local/bin/rusty-mirror   # installer copy
cargo uninstall rusty-mirror-cli                               # Cargo copy
```

Cargo manages its own download cache under `~/.cargo`.
