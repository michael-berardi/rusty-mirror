# `rusty-mirror` CLI reference

Every command prints exactly one JSON document on stdout. Human chatter, when
there is any, goes to stderr.

| Exit | Meaning |
| --- | --- |
| `0` | Did what you asked |
| `1` | Failed: transport error, refused request, a script that threw, a blank capture |
| `2` | Usage error |
| `3` | `audit` found mirror code or mirror permissions |

## Choosing the app

`--app <identifier>` (or `RUSTY_MIRROR_APP`) selects the app. Omit it when
exactly one mirror-enabled app is running. `--socket` / `RUSTY_MIRROR_SOCKET`
overrides the socket path. `RUSTY_MIRROR_HOME` moves the whole root (default
`~/.rusty-mirror`); keep it short, as unix socket paths top out near 104 bytes.

## Commands

| Command | What happens |
| --- | --- |
| `doctor` | Platform, root, running apps, and the selected app's state |
| `state` | Open? Visible? Focused? URL, revision, size, monitors, seconds left |
| `open [--seconds N] [--size WxH]` | Snapshot the primary, open the invisible mirror. N is 1–3600 (default 900) |
| `eval BODY` / `eval -f FILE` / `eval -` | Run an async function body in the mirror. `return` a JSON value |
| `capture OUT.png` | Native snapshot of the hidden webview. Never overwrites; flags flat images |
| `resize WxH` | Logical size, 200x200 to 7680x4320 |
| `screen N` | Move the invisible mirror to monitor N to test another scale factor |
| `stage DIST` | Copy a built frontend into an immutable, hashed revision. Offline |
| `refresh REV` | Rebuild the mirror on a staged revision, carrying its own state |
| `rollback` | Refresh back to the previous revision (mirror only) |
| `apply` | Reload the primary window on the mirror's verified revision |
| `close` | Destroy the mirror and wait until it is really gone |
| `run [--seconds N] -- CMD…` | Open, run CMD with `RUSTY_MIRROR_APP` set, always close |
| `audit PATH… [--capabilities DIR]` | Scan builds and capability files for anything mirror-shaped |

### `eval`

The body runs inside `async () => { … }`, after finite animations are
finished (hidden WebKit pauses them). Examples:

```sh
rusty-mirror eval 'return document.querySelectorAll("[role=dialog]").length'
rusty-mirror eval 'document.querySelector("#settings").click(); await new Promise(r => requestAnimationFrame(r)); return location.hash'
rusty-mirror eval -f checks/focus-ring.js
```

Output is `{"ok": true, "value": …}` or `{"ok": false, "error": "…"}` with the
exit code to match. The limit is 32 KiB of script and 8 seconds of runtime.

### `capture`

```json
{ "ok": true, "path": "shots/a.png", "renderer": "WKWebView.takeSnapshot",
  "mirrorVisible": false,
  "image": { "width": 840, "height": 1120, "distinctColoursSampled": 412,
             "dominantColourShare": 0.71, "looksBlank": false } }
```

`looksBlank` is a floor, not a verdict: a non-blank PNG can still be the wrong
screen. Open the image and look.

### `stage`

Refuses symlinks, hidden or key-like files, absolute asset URLs, files over
16 MiB, builds over 64 MiB or 4096 files, and any build without the snapshot
hook (that is a consumer build, and staging it would test the wrong thing).
Staging the same bytes twice returns the existing revision.

## Wire protocol

Newline-delimited JSON on a same-user unix socket, one request per
connection:

```json
{"v":1,"action":"open","seconds":600}
{"v":1,"action":"eval","script":"return 1"}
{"v":1,"action":"refresh","revision":"<64 hex>"}
```

Responses are `{"ok":true,"result":…}` or `{"ok":false,"error":"…"}`. Unknown
actions and unknown fields are refused. Requests are limited to 64 KiB.
Types live in `rusty-mirror-core::protocol`.
