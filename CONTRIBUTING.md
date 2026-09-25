# Contributing

Thanks for considering it. Small, focused pull requests are easiest to review.

## Checks

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --features tally/mirror-debug
cargo test
scripts/e2e.sh        # macOS: builds Tally, runs it off-screen, drives every command
```

The end-to-end script launches only its own hidden app, keeps everything in a
temporary directory and removes it afterwards. It never shows a window.

## Wanted

- Native capture for WebKitGTK (Linux) and WebView2 (Windows)
- A Windows control transport (named pipe)
- More adopters' war stories in `docs/`

## Ground rules

- Keep the mirror invisible, unfocused and effect-denied. Changes that weaken
  any layer in [docs/security.md](./docs/security.md) need a very good reason
  and a test.
- Nothing may leak into builds without the `mirror-debug` feature. If you add
  a marker, add it to `rusty-mirror-core::audit::MARKERS`.
- CI is local. Run the checks above before opening a PR.
