# Security model

## What Rusty Mirror is for

Letting your own trusted frontend code, and agents acting for you, exercise a
real app window without that window being able to change anything that
matters. The adversary is accident, not malice: a Save button that saves, a
refresh that fetches, a test that deletes the thing it was testing.

## What it is not

- **Not an OS sandbox.** The mirror lives in your app's process. Native code
  reachable through an allowlisted command runs with your app's full rights.
- **Not a boundary against hostile JavaScript.** Anything your frontend could
  do in a webview, a script in the mirror could attempt; the layers below
  decide how far it gets.
- **Not for consumer builds.** It is compiled out of them, and must stay out.

## The layers

| Layer | Enforced by | Stops |
| --- | --- | --- |
| Feature gate | Cargo + your frontend build | Everything, in consumer builds |
| `guard` allowlist | Native invoke handler | App commands you did not allow |
| Capabilities | Tauri ACL | Plugin commands (fs, shell, http…) if no capability names `rusty-mirror` |
| Mirror CSP | WebKit | Outbound fetch, XHR, sockets, images, frames, forms |
| Navigation lock | Webview `on_navigation` | Leaving the app origin or dropping the mirror query |
| Storage shim + incognito | Init script, WebKit data store | Persistent writes to storage |
| `window.open` | Init script | Popups |
| Socket | 0700 dir, 0600 socket, peer UID | Other local users |
| Deadline | Plugin timer | Forgotten mirrors (1–3600 s) |

## Edges worth knowing

- **Allowlist honestly.** A command that reads today and writes tomorrow will
  write from the mirror too. Prefer small, obviously read-only commands.
- **Capabilities are yours to get right.** Rusty Mirror cannot see Tauri's
  resolved ACL at runtime; `rusty-mirror audit --capabilities` checks the
  JSON files. TOML capabilities and capabilities added in code are not
  scanned.
- **The primary is read, and on `apply`, reloaded.** `open` runs a snapshot script in the primary; `refresh` snapshots the
  mirror instead. Snapshot hooks are application code and must be read-only. `apply` navigates it after your
  `__RUSTY_MIRROR_BEFORE_APPLY__` hook agrees. Nothing else touches it.
- **CSP allows same-origin requests.** The default policy allows `self` and
  Tauri IPC. It does not deny every network request, especially with a dev
  server, and a custom `.csp(...)` can weaken it.
- **Audits are marker scans.** Binary scans skip symlinks; capability scans
  inspect JSON window/webview patterns. A clean result is not a security proof.
- **Captures are sensitive.** They show whatever your app shows. The CLI writes
  them 0600, creates missing parent directories 0700 and never overwrites; where
  they go, and how long they live, is up to you. Delete them when done.
- **Staged revisions are code.** They load only from your private root and are
  re-hashed on load, but anyone who can write there as you can already run
  code as you.
- **Same-user trust.** Any process running as you can talk to the socket. That
  is the same trust boundary as your app's data directory.

## Reporting

See [SECURITY.md](../SECURITY.md).
