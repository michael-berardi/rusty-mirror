# Tally

The smallest honest Rusty Mirror integration: a counter with one read
(`get_tally`, allowed in the mirror) and one effect (`save_tally`, which
writes to disk and is refused in the mirror).

```sh
RUSTY_MIRROR=1 ./build.sh                          # frontend with the mirror gate on
cargo run -p tally --features mirror-debug         # run it (add TALLY_HIDDEN=1 to stay off-screen)
rusty-mirror --app dev.rustymirror.tally open      # in another terminal
```

`scripts/e2e.sh` at the repository root does all of this unattended and
checks every command. Worth reading as a tour.

Where the mirror is wired in:

- `src-tauri/src/main.rs`: three `#[cfg(feature = "mirror-debug")]` blocks
- `src/app.js`: the fenced snapshot hook, stripped from consumer builds by `build.sh`
- `src-tauri/capabilities/main.json`: names `main` only, never `*`
