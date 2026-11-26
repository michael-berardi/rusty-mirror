# Rusty Mirror

[![Latest release](https://img.shields.io/github/v/release/michael-berardi/rusty-mirror?label=release)](https://github.com/michael-berardi/rusty-mirror/releases/latest) [![MIT License](https://img.shields.io/github/license/michael-berardi/rusty-mirror)](LICENSE) ![Tauri 2.5+](https://img.shields.io/badge/Tauri-2.5%2B-24C8DB) ![Current stable Rust](https://img.shields.io/badge/rust-stable-orange)

Rusty Mirror gives Tauri developers and UI-testing agents a hidden second
window inside their app. It uses the app's native webview and frontend,
seeded with a snapshot of the primary window's state. Inspect the DOM, take
native screenshots on macOS, and try rebuilt frontends before reloading the
primary window.

The mirror rejects app commands outside your allowlist. It is development
tooling for trusted code, not an OS sandbox: allowlisted native commands
still have the app's full permissions.

## Install

Install the CLI in one line on macOS 14+ or Linux x86_64:

```sh
curl -fsSL https://github.com/michael-berardi/rusty-mirror/releases/latest/download/install.sh | sh
```

The installer fetches `SHA256SUMS` and checks the archive's SHA-256 before it
unpacks anything. It installs the CLI into `~/.rusty-mirror/releases/<version>`
and writes a shim to `~/.local/bin/rusty-mirror`; add that directory to your
PATH if needed. It will not replace a `rusty-mirror` it did not install. To
change the destination or select a build, set `RUSTY_MIRROR_HOME`,
`RUSTY_MIRROR_BIN_DIR`, `RUSTY_MIRROR_VERSION` (default: latest) or
`RUSTY_MIRROR_RELEASE_BASE` (a local release directory, an `http(s)://` or
`file://` URL, used by tests). The macOS build is a universal binary, Developer ID signed and
notarized; the Linux x86_64 build is a plain binary.

Alternatively, install the CLI with Cargo:

```sh
cargo install --git https://github.com/michael-berardi/rusty-mirror --tag v0.3.0 --locked rusty-mirror-cli
rusty-mirror --help
```

Use a current stable Rust toolchain and Tauri 2.5 or later. The workspace
manifest declares Rust 1.80, but the locked dependencies include crates
requiring Rust 1.88; Rust 1.80 is not sufficient for this checkout.

The CLI needs a mirror-enabled build of your app. Add the optional library
plugin to your Tauri crate:

```toml
# src-tauri/Cargo.toml
[features]
mirror-debug = ["dep:rusty-mirror"]

[dependencies]
rusty-mirror = { git = "https://github.com/michael-berardi/rusty-mirror", tag = "v0.3.0", optional = true }
```

Follow [the integration guide](docs/integrate.md) to install the asset
wrapper, guard your invoke handler, register the plugin, and supply a
frontend snapshot hook. Keep both Rust and frontend mirror code behind
non-default build gates. [Tally](examples/tally) is a complete example.

## First use

With your mirror-enabled app running:

```sh
rusty-mirror open --seconds 600
rusty-mirror eval 'return document.title'
rusty-mirror capture shots/before.png
rusty-mirror close
```

The window stays hidden and unfocused. Captures never overwrite files;
inspect each image rather than treating its existence as a passed check.
Use `--app <identifier>` when multiple apps have control sockets.

To test a rebuilt frontend, build it with its mirror gate enabled and a
relative asset base, then run:

```sh
rusty-mirror open
rusty-mirror stage dist
rusty-mirror refresh <revision>       # use the digest returned by stage
rusty-mirror eval 'return document.readyState'
rusty-mirror capture shots/after.png
rusty-mirror close
```

Refresh needs embedded assets (`custom-protocol`), not a dev-server URL.
It carries the mirror's state forward; navigation is asynchronous, so check
an app-specific readiness condition before capturing. `rollback` restores
the previous mirror revision. `apply` reloads the primary on the staged
revision: only do this after inspection and when reloading is safe. It does
not copy mirror storage or replace native code.

`rusty-mirror run --seconds 600 -- ./qa/visual-check.sh` opens a mirror,
waits for your script, then attempts to close it. The finite lifetime is a
backstop, not a replacement for cleanup. See the [CLI reference](docs/cli.md)
for commands, output and exit codes, or the [agent skill](skills/rusty-mirror).

## Screenshots

Bundled Tally captures show count 3 before and after a frontend refresh:

<p align="center">
  <img src="docs/img/mirror-v1.png" alt="Tally rendered in a hidden mirror: count 3, build v1" width="280" />
  <img src="docs/img/mirror-v2-wide.png" alt="The same mirror after a hot refresh to build v2 at 1280x800, count still 3" width="520" />
</p>

## Safety and platforms

- Allow only read-only app commands, and wrap the invoke handler with `guard`.
- Keep `rusty-mirror` out of Tauri capabilities, including wildcard grants.
- The default CSP restricts network access; storage is memory-backed in an
  incognito webview. Custom policies and same-origin requests need care.
- Control uses a same-user Unix socket. Any process running as you can use it.
- Mirrors expire after 1–3600 seconds (default 900).
- Audit consumer frontends and binaries with `rusty-mirror audit`, but treat
  marker scanning as a check, not a proof of complete isolation.

See the [security model](docs/security.md) and [reporting policy](SECURITY.md).

| Platform | Status |
| --- | --- |
| macOS 14+ | Native mirror and `WKWebView.takeSnapshot` capture; no Screen Recording permission needed. Signed, notarized universal CLI download |
| Linux x86_64 | CLI download; core and CLI use Unix APIs, but plugin and capture support are not validated here |
| Windows | Unsupported: no socket transport, no Windows installer |

## Updates

On a terminal, the CLI tells you once a day when a newer release exists.
`rusty-mirror update` installs it with Cargo, and automatic installs stay off
until you run `rusty-mirror update --auto on`. See [updates](docs/updates.md).

## Privacy

- No telemetry, analytics or identifiers. Mirrors and captures stay on your machine.
- The only request made on its own is the daily update check: one anonymous GET of the public release record.
- It is off in CI, agent shells and non-terminal use, and with `RUSTY_MIRROR_NO_UPDATE_CHECK=1`.
- Details: [PRIVACY.md](PRIVACY.md).

## Contributing

From a fresh checkout:

```sh
examples/tally/build.sh
cargo fmt --all --check
cargo test -j4
cargo clippy --workspace --all-targets --features tally/mirror-debug -j4
```

The frontend build is required because the default workspace includes Tally.
On macOS, `CARGO_BUILD_JOBS=4 scripts/e2e.sh` builds and **launches** the example
app off-screen to test the native workflow. `cargo test -p rusty-mirror-cli
--test install` exercises the installer against a local release directory with
a temporary HOME. Release assets are built on a Mac with
`scripts/build-release.sh`, which takes the Developer ID identity from
`RUSTY_MIRROR_SIGNING_IDENTITY` and the notarytool keychain profile from
`NOTARY_PROFILE`; neither belongs in a committed file. See
[CONTRIBUTING.md](CONTRIBUTING.md).

## License

[MIT](LICENSE).

---

Part of [Pareto Cybernetics](https://paretocybernetics.com/projects/rusty-mirror/): free, private, open-source tools for working with AI agents.
