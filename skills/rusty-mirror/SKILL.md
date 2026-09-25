---
name: rusty-mirror
description: Develop, visually debug and QA Rust desktop apps (Tauri) through Rusty Mirror, a hidden, disposable, effect-denied twin of the real app window with the same native renderer, bundle and state. Use for native UI changes, dialog/preview polish, visual regression checks, hot-refreshing a frontend build before applying it, and release verification. Use it instead of opening the frontend in a browser or building an HTML stand-in.
---

# Rusty Mirror

The mirror is the real application's native webview, running the real
frontend bundle, seeded from a snapshot of the live primary window. It is not
a screenshot, a browser tab, a dev-server page, or a hand-built mock. Treat
evidence from any of those as evidence about something else.

## Contract

- **Invisible and unfocused, always.** Never show, raise, activate or focus the
  mirror to make something work. If a capture fails, diagnose with `eval`.
- **Effects denied, controls alive.** Clicks, typing, scrolling, dialogs and
  settings work locally in the mirror. Saving, sending, deleting, spawning
  and network calls are refused natively. A disabled-looking UI or a frozen
  screenshot does not meet the contract.
- **The primary is not yours.** Only `apply` changes it, and only after the
  mirror has been verified and the operator's workflow allows it.
- **Close every session**, including failed ones: `rusty-mirror close`, then
  `state` must say `"open": false`. Prefer `rusty-mirror run -- <script>`,
  which closes on success, failure and interruption.

## Before you start

1. Read the project's agent instructions and find its mirror build command
   (typically a `mirror-debug` Cargo feature plus a frontend flag).
2. `rusty-mirror doctor`. It must show the app and `"open": false`. If the
   socket is missing, the running build lacks the feature; source changes
   alone do not add it.
3. Record the primary's size, URL and anything else you must not disturb.

## Loop

```sh
rusty-mirror open --seconds 900
rusty-mirror eval 'return {ready: document.readyState, title: document.title}'
rusty-mirror capture /tmp/screenshots/<task>-before.png
# fix the real source, rebuild the frontend with the mirror gate on
rusty-mirror stage <dist>
rusty-mirror refresh <revision>          # navigation is async: poll with eval
rusty-mirror capture /tmp/screenshots/<task>-after.png
rusty-mirror close
```

- Wait for real app readiness (a known control exists, no splash), not just
  `document.readyState`.
- Assert the intended state exists (the dialog is open, the pane is present)
  before accepting a capture as evidence of that state.
- Keep viewport, theme and content identical between before and after.
- Batch independent small fixes between rebuilds; keep a short defect ledger.
- `rollback` is mirror-first. Verify, then `apply` only if authorised.
- Native (Rust) changes need a rebuild and relaunch of the debug app. Never
  claim native code was hot-swapped.

## Honest visual proof

1. Prove the mirror works first: opens, renders the real app, accepts a local
   click, stays invisible and unfocused, closes cleanly. No polish claims
   while the mirror itself is broken.
2. Open and inspect every capture individually at native resolution. A file
   that exists, or `looksBlank: false`, is not a passed check.
3. Exercise real states: long titles, empty/loading/error states, scrolled
   and pinned lists, light and dark themes, each supported window size.
   Fixtures may fill gaps; label them as fixtures.
4. Hidden WebKit caveats:
   - Finite CSS/WAAPI animations are finished before each eval and capture.
     That is settled-state proof, not motion or performance proof.
   - `document.activeElement` can move while `:focus` and `:focus-visible`
     stay false in a hidden window. A ring-free hidden capture is not proof
     of correct focus styling. Verify focus in the real app.
5. When delegating visual review, use a reviewer that can actually see images
   and hand it the image files, not descriptions of them.

## Shipping boundary

The mirror is private development tooling. Before any release:

```sh
rusty-mirror audit <frontend-dist> <release-binary-or-.app> --capabilities src-tauri/capabilities
```

Exit `0` is required. Also run the normal consumer build's tests; a QA build
passing is not proof about the consumer artifact.

## Reporting

State what you inspected (with capture paths), what you fixed, what still
fails, the audit result, and cleanup evidence (`state` showing closed, no
leftover processes you started). Never write "pixel perfect" or "flawless"
on the strength of tests alone.
