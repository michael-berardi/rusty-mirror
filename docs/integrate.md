# Integrating Rusty Mirror

About fifteen minutes, most of which is deciding which commands are
read-only. [Tally](../examples/tally) is the complete worked example; every
snippet below comes from it or was checked against it.

## 1. Add a private feature

```toml
# src-tauri/Cargo.toml
[features]
default = ["custom-protocol"]
custom-protocol = ["tauri/custom-protocol"]
mirror-debug = ["dep:rusty-mirror"]

[dependencies]
rusty-mirror = { git = "https://github.com/michael-berardi/rusty-mirror", tag = "v0.1.0", optional = true }
```

`mirror-debug` must never be a default feature. Hot refresh serves embedded
assets, so debug builds need `custom-protocol` (a `devUrl` dev server works for
`open`/`eval`/`capture`, but `refresh` and `apply` will politely refuse).

## 2. Wire the native side

```rust
fn main() {
    #[allow(unused_mut)]
    let mut context = tauri::generate_context!();
    #[cfg(feature = "mirror-debug")]
    rusty_mirror::install_hot_assets(&mut context);

    // A fn pointer keeps the handler's type nameable with or without the feature.
    let handler: fn(tauri::ipc::Invoke) -> bool = tauri::generate_handler![get_tally, save_tally];
    #[cfg(feature = "mirror-debug")]
    let handler = rusty_mirror::guard(handler);

    let builder = tauri::Builder::default();
    #[cfg(feature = "mirror-debug")]
    let builder = builder.plugin(rusty_mirror::Builder::new().allow(["get_tally"]).build());

    builder.invoke_handler(handler).run(context).expect("app ran");
}
```

Builder options, all optional:

| Method | Default | Purpose |
| --- | --- | --- |
| `.allow([...])` | nothing | App commands the mirror may call. Read-only ones. |
| `.primary("main")` | `main` | Label of the window being mirrored |
| `.app_id("...")` | Tauri `identifier` | Directory under `~/.rusty-mirror/` |
| `.size(w, h)` | primary's size | Initial logical size |
| `.csp("...")` | `DEFAULT_CSP` | Replace the mirror-only content security policy |

Commands that should *appear* to work in the mirror (a "Save" that shows a
toast, say) can check `rusty_mirror::is_mirror(&webview)` and simulate. Keep
that inside the feature gate. Disable effects, not controls.

## 3. Keep capabilities off the mirror

Every capability file should name its windows explicitly:

```json
{ "identifier": "main", "windows": ["main"], "permissions": ["core:default"] }
```

`"windows": ["*"]` would hand the mirror every plugin permission you own.
Check with:

```sh
rusty-mirror audit --capabilities src-tauri/capabilities
```

## 4. Hand over state from the frontend

The mirror always receives a copy of the primary's `localStorage` and
`sessionStorage`. For in-memory state (stores, selections, open panels), define
a snapshot hook in debug builds and read the seed back:

```js
if (__RUSTY_MIRROR__) {
  window.__RUSTY_MIRROR_SNAPSHOT__ = () => store.getSnapshot();  // must be JSON-serialisable; may be async
  const seed = window.__RUSTY_MIRROR_SEED__;                     // present only inside a mirror
  if (seed?.state) store.hydrate(seed.state);
}
```

`seed` is `{ state, hooked, storage: { local, session } }`. Hydrate what makes
the view identical; skip what would start work (timers that poll, sockets that
connect). The mirror's CSP will refuse outbound connections anyway, but a quiet
mirror is easier to reason about.

### Vite

```js
// vite.config.js
export default defineConfig({
  base: './', // staged revisions are served from a sub-path
  define: { __RUSTY_MIRROR__: JSON.stringify(process.env.RUSTY_MIRROR === '1') },
});
```

`RUSTY_MIRROR=1 vite build` keeps the block; a plain build folds
`if (false)` away, strings and all. Verified with Vite 7: zero markers in the
consumer bundle.

### No bundler

Fence the block with comments and strip it in your build script, as Tally
does:

```sh
sed '/rusty-mirror:begin/,/rusty-mirror:end/d' src/app.js > dist/app.js
```

## 5. Optional: veto an apply

`apply` reloads the primary window. If that would lose something (a draft, a
queued message, an open dialog), say so:

```js
window.__RUSTY_MIRROR_BEFORE_APPLY__ = () =>
  composer.value.trim() ? 'Finish or clear the draft before reloading' : true;
```

Return `true` (or nothing) to allow; return a string or throw to refuse. It is
also the right moment to stash view state in `sessionStorage` for the reload.

## 6. Prove the consumer build is clean

```sh
vite build && cargo build --release
rusty-mirror audit dist target/release/your-app --capabilities src-tauri/capabilities
```

Exit `0` means no mirror markers anywhere. Exit `3` means something leaked;
do not ship it. Run the same audit against the signed `.app` if you have one.
