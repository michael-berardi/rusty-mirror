//! The mirror itself: open, evaluate, capture, refresh, apply, close.
use rusty_mirror_core::{
    protocol::Request, BEFORE_APPLY_GLOBAL, HOT_PREFIX, LABEL, QUERY, SEED_GLOBAL, SNAPSHOT_GLOBAL,
};
use serde_json::{json, Value};
use std::{
    sync::{mpsc, Mutex},
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindow, Wry};

const EVAL_TIMEOUT: Duration = Duration::from_secs(8);

struct Session {
    generation: String,
    deadline: Instant,
    current: Option<String>,
    previous: Option<String>,
}

static SESSION: Mutex<Option<Session>> = Mutex::new(None);

fn session<T>(f: impl FnOnce(&mut Option<Session>) -> T) -> T {
    let mut guard = SESSION.lock().unwrap_or_else(|p| p.into_inner());
    f(&mut guard)
}

/// Hidden WebKit pauses finite CSS/WAAPI transitions along with frames.
/// Finish them — in the mirror only — so reads and captures see settled layout.
const SETTLE: &str = "for (let pass = 0; pass < 2; pass++) { let changed = false; \
for (const a of document.getAnimations?.() ?? []) { if (a.playState !== 'finished' && \
Number.isFinite(a.effect?.getComputedTiming().endTime)) { try { a.finish(); changed = true } catch {} } } \
if (!changed) break; document.documentElement.getBoundingClientRect(); }";

/// Run an async function body and return `{ok, value}` or `{ok:false, error}`.
///
/// No IPC is involved: the result is parked on `window` and collected with the
/// webview's native evaluate callback, so the mirror needs no permissions.
pub(crate) fn evaluate(window: &WebviewWindow<Wry>, body: &str) -> Result<Value, String> {
    let nonce = serde_json::to_string(&uuid::Uuid::new_v4().to_string()).unwrap();
    let settle = if window.label() == LABEL { SETTLE } else { "" };
    let kickoff = format!(
        "(() => {{ const slot = (window.__rustyMirrorEval ||= Object.create(null)); const id = {nonce}; \
slot[id] = {{ done: false }}; \
(async () => {{ {settle}\n return await (async () => {{\n{body}\n}})(); }})().then(\
v => {{ let json; try {{ json = JSON.stringify({{ ok: true, value: v === undefined ? null : v }}) }} \
catch (e) {{ json = JSON.stringify({{ ok: false, error: 'result is not JSON-serializable: ' + e }}) }} \
slot[id] = {{ done: true, json }} }}, \
e => {{ slot[id] = {{ done: true, json: JSON.stringify({{ ok: false, error: String(e) + (e && e.stack ? '\\n' + e.stack : '') }}) }} }}); \
return true; }})()"
    );
    let started = native_eval(window, &kickoff, EVAL_TIMEOUT)?;
    if started != "true" {
        return Err("script did not start (syntax error, or the page is still loading)".into());
    }
    let poll = format!(
        "(() => {{ const slot = window.__rustyMirrorEval; const r = slot && slot[{nonce}]; \
if (!r || !r.done) return null; delete slot[{nonce}]; return r.json; }})()"
    );
    let deadline = Instant::now() + EVAL_TIMEOUT;
    while Instant::now() < deadline {
        let raw = native_eval(
            window,
            &poll,
            deadline.saturating_duration_since(Instant::now()),
        )?;
        if !raw.is_empty() && raw != "null" {
            let json: String =
                serde_json::from_str(&raw).map_err(|e| format!("eval transport: {e}"))?;
            return serde_json::from_str(&json).map_err(|e| format!("eval result: {e}"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let _ = window.eval(format!("delete window.__rustyMirrorEval?.[{nonce}]"));
    Err("eval timed out after 8s (a hidden page may be waiting on a suspended timer)".into())
}

fn native_eval(window: &WebviewWindow<Wry>, js: &str, timeout: Duration) -> Result<String, String> {
    let (tx, rx) = mpsc::sync_channel::<String>(1);
    window
        .eval_with_callback(js, move |result| {
            let _ = tx.try_send(result);
        })
        .map_err(|e| e.to_string())?;
    rx.recv_timeout(timeout)
        .map_err(|_| "webview did not answer".to_string())
}

/// Read the primary's (or mirror's) state and storage. Read-only by design.
const SNAPSHOT: &str = "const dump = s => { const o = {}; try { for (let i = 0; i < s.length; i++) { \
const k = s.key(i); if (k !== null) o[k] = s.getItem(k); } } catch {} return o; }; \
const hook = window.__RUSTY_MIRROR_SNAPSHOT__; \
return { state: typeof hook === 'function' ? (await hook()) ?? null : null, hooked: typeof hook === 'function', \
storage: { local: dump(localStorage), session: dump(sessionStorage) } };";

fn snapshot(window: &WebviewWindow<Wry>) -> Result<Value, String> {
    debug_assert!(SNAPSHOT.contains(SNAPSHOT_GLOBAL));
    let result = evaluate(window, SNAPSHOT)?;
    if result["ok"] != true {
        return Err(format!(
            "snapshot failed: {}",
            result["error"].as_str().unwrap_or("unknown")
        ));
    }
    Ok(result["value"].clone())
}

fn primary() -> Result<String, String> {
    Ok(crate::config()?.primary.clone())
}

fn init_script(seed: &Value, csp: &str) -> String {
    let seed = serde_json::to_string(seed).unwrap_or_else(|_| "null".into());
    let csp = serde_json::to_string(csp).unwrap();
    format!(
        r#"window.{SEED_GLOBAL} = {seed};
(() => {{
  // Mirror storage lives in memory, seeded from the primary. Nothing persists.
  const memory = (init) => {{
    const m = new Map(Object.entries(init || {{}}));
    return {{
      get length() {{ return m.size }},
      key(i) {{ return [...m.keys()][i] ?? null }},
      getItem(k) {{ k = String(k); return m.has(k) ? m.get(k) : null }},
      setItem(k, v) {{ m.set(String(k), String(v)) }},
      removeItem(k) {{ m.delete(String(k)) }},
      clear() {{ m.clear() }},
    }};
  }};
  const seed = window.{SEED_GLOBAL};
  for (const [name, init] of [['localStorage', seed?.storage?.local], ['sessionStorage', seed?.storage?.session]]) {{
    try {{ Object.defineProperty(window, name, {{ value: memory(init), configurable: true }}) }} catch {{}}
  }}
  window.open = () => null;
  // JS fetch guards miss images, beacons, sockets and forms. A CSP does not.
  const lock = () => {{
    if (!document.head) return false;
    const meta = document.createElement('meta');
    meta.httpEquiv = 'Content-Security-Policy';
    meta.content = {csp};
    document.head.prepend(meta);
    return true;
  }};
  if (!lock()) {{
    const watch = new MutationObserver(() => {{ if (lock()) watch.disconnect() }});
    watch.observe(document, {{ childList: true, subtree: true }});
  }}
}})();"#
    )
}

/// Path (relative to the app origin) the mirror should load for `revision`.
fn mirror_path(app: &AppHandle<Wry>, revision: Option<&str>) -> Result<String, String> {
    let path = match revision {
        Some(rev) => format!("{}{rev}/index.html", &HOT_PREFIX[1..]),
        None => {
            let main = app
                .get_webview_window(&primary()?)
                .ok_or("primary window unavailable")?;
            let url = main.url().map_err(|e| e.to_string())?;
            let p = url.path().trim_start_matches('/');
            if p.is_empty() {
                "index.html".into()
            } else {
                p.to_owned()
            }
        }
    };
    Ok(format!("{path}?{QUERY}"))
}

fn build(
    app: &AppHandle<Wry>,
    seed: &Value,
    path: &str,
    width: f64,
    height: f64,
) -> Result<(), String> {
    let config = crate::config()?;
    let main = app
        .get_webview_window(&config.primary)
        .ok_or("primary window unavailable")?;
    let origin = main.url().map_err(|e| e.to_string())?;
    let (scheme, host, port) = (
        origin.scheme().to_owned(),
        origin.host_str().map(str::to_owned),
        origin.port(),
    );
    let window = tauri::WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App(path.into()))
        .title("Rusty Mirror")
        .inner_size(width, height)
        .decorations(false)
        .visible(false)
        .focused(false)
        .skip_taskbar(true)
        .incognito(true)
        .initialization_script(init_script(seed, &config.csp))
        .on_navigation(move |url| {
            url.scheme() == scheme
                && url.host_str() == host.as_deref()
                && url.port() == port
                && url
                    .query()
                    .is_some_and(|q| q.split('&').any(|kv| kv == QUERY))
        })
        .build()
        .map_err(|e| e.to_string())?;
    #[cfg(target_os = "macos")]
    window
        .with_webview(|webview| unsafe { crate::macos::keep_scheduling(webview.inner()) })
        .map_err(|e| e.to_string())?;
    let _ = window;
    Ok(())
}

fn mirror(app: &AppHandle<Wry>) -> Result<WebviewWindow<Wry>, String> {
    app.get_webview_window(LABEL)
        .ok_or_else(|| "no mirror is open; run `rusty-mirror open` first".into())
}

fn open(app: &AppHandle<Wry>, seconds: u64, size: Option<(u32, u32)>) -> Result<Value, String> {
    if app.get_webview_window(LABEL).is_some() {
        return Err("a mirror is already open; close it or keep using it".into());
    }
    let config = crate::config()?;
    let main = app
        .get_webview_window(&config.primary)
        .ok_or("primary window unavailable")?;
    let seed = snapshot(&main)?;
    let (width, height) = match (size, config.size) {
        (Some((w, h)), _) => (w as f64, h as f64),
        (None, Some(s)) => s,
        (None, None) => {
            let scale = main.scale_factor().map_err(|e| e.to_string())?;
            let s = main.inner_size().map_err(|e| e.to_string())?;
            (s.width as f64 / scale, s.height as f64 / scale)
        }
    };
    build(app, &seed, &mirror_path(app, None)?, width, height)?;
    let generation = uuid::Uuid::new_v4().to_string();
    session(|s| {
        *s = Some(Session {
            generation: generation.clone(),
            deadline: Instant::now() + Duration::from_secs(seconds),
            current: primary_revision(app),
            previous: None,
        })
    });
    let handle = app.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(seconds));
        session(|s| {
            if s.as_ref().is_some_and(|s| s.generation == generation) {
                if let Some(w) = handle.get_webview_window(LABEL) {
                    let _ = w.destroy();
                }
                *s = None;
            }
        });
    });
    let mut warnings = vec![];
    if seed["hooked"] != true {
        warnings.push(format!(
            "primary defines no window.{SNAPSHOT_GLOBAL}; the mirror got storage but no app state"
        ));
    }
    Ok(json!({
        "label": LABEL, "visible": false, "focused": false, "seconds": seconds,
        "size": [width, height], "stateHooked": seed["hooked"] == true, "warnings": warnings,
        "next": "eval/capture are async-safe; always finish with `close`",
    }))
}

fn state(app: &AppHandle<Wry>) -> Result<Value, String> {
    let config = crate::config()?;
    let base = json!({
        "appId": config.app_id, "primary": config.primary, "allow": config.allow,
        "hotRefresh": crate::hot::installed(), "nativeCapture": cfg!(target_os = "macos"),
    });
    let Some(window) = app.get_webview_window(LABEL) else {
        return Ok(json!({ "open": false, "info": base }));
    };
    let (current, previous, remaining) = session(|s| match s {
        Some(s) => (
            s.current.clone(),
            s.previous.clone(),
            s.deadline
                .saturating_duration_since(Instant::now())
                .as_secs(),
        ),
        None => (None, None, 0),
    });
    let scale = window.scale_factor().ok();
    let size = window
        .inner_size()
        .ok()
        .zip(scale)
        .map(|(s, f)| [s.width as f64 / f, s.height as f64 / f]);
    let monitors = window.available_monitors().ok().map(|ms| {
        ms.into_iter()
            .enumerate()
            .map(|(i, m)| json!({"index": i, "name": m.name(), "scaleFactor": m.scale_factor(), "size": m.size()}))
            .collect::<Vec<_>>()
    });
    Ok(json!({
        "open": true, "info": base,
        "visible": window.is_visible().unwrap_or(true), "focused": window.is_focused().unwrap_or(true),
        "url": window.url().ok().map(|u| u.to_string()),
        "primaryUrl": app.get_webview_window(&config.primary).and_then(|w| w.url().ok()).map(|u| u.to_string()),
        "revision": current.unwrap_or_else(|| "embedded".into()), "previousRevision": previous,
        "secondsLeft": remaining, "size": size, "scaleFactor": scale, "monitors": monitors,
    }))
}

fn capture(window: &WebviewWindow<Wry>) -> Result<Value, String> {
    let settled = evaluate(window, "return true")?;
    if settled["ok"] != true {
        return Err("mirror layout did not settle".into());
    }
    #[cfg(target_os = "macos")]
    {
        use base64::Engine;
        let (tx, rx) = mpsc::sync_channel(1);
        window
            .with_webview(move |webview| unsafe {
                crate::macos::take_snapshot(webview.inner(), tx)
            })
            .map_err(|e| e.to_string())?;
        let png = rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "hidden WKWebView snapshot timed out; the window stays hidden. Use eval to inspect the DOM.")??;
        Ok(json!({
            "renderer": "WKWebView.takeSnapshot", "visible": window.is_visible().unwrap_or(true),
            "bytes": png.len(), "png": base64::engine::general_purpose::STANDARD.encode(png),
        }))
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = window;
        Err("native capture is macOS-only for now; use eval to inspect the DOM (contributions welcome)".into())
    }
}

/// Rebuild the mirror on `revision`, carrying its own (not the primary's) state.
fn refresh(app: &AppHandle<Wry>, revision: &str) -> Result<Value, String> {
    let config = crate::config()?;
    let main = app
        .get_webview_window(&config.primary)
        .ok_or("primary window unavailable")?;
    let origin = main.url().map_err(|e| e.to_string())?;
    if !(origin.scheme() == "tauri" || origin.host_str() == Some("tauri.localhost")) {
        return Err(
            "hot refresh needs embedded assets (custom-protocol build), not a dev server URL"
                .into(),
        );
    }
    let window = mirror(app)?;
    if revision != "embedded" {
        crate::hot::load(&config.app_id, revision)?;
    }
    let seed = snapshot(&window)?;
    let scale = window.scale_factor().map_err(|e| e.to_string())?;
    let size = window.inner_size().map_err(|e| e.to_string())?;
    window.destroy().map_err(|e| e.to_string())?;
    let path = if revision == "embedded" {
        mirror_path(app, None)?
    } else {
        mirror_path(app, Some(revision))?
    };
    wait_gone(app)?; // the label must be free before it is reused
    build(
        app,
        &seed,
        &path,
        size.width as f64 / scale,
        size.height as f64 / scale,
    )?;
    session(|s| {
        if let Some(s) = s {
            let old = s.current.clone().unwrap_or_else(|| "embedded".into());
            if old != revision {
                s.previous = Some(old);
            }
            s.current = (revision != "embedded").then(|| revision.to_owned());
        }
    });
    Ok(json!({
        "revision": revision, "primaryUntouched": true,
        "verify": "navigation is asynchronous: check state, then eval/capture the new UI",
    }))
}

fn apply(app: &AppHandle<Wry>) -> Result<Value, String> {
    let config = crate::config()?;
    mirror(app)?;
    let revision = session(|s| s.as_ref().and_then(|s| s.current.clone()))
        .ok_or("the mirror is on the embedded build; refresh it to a staged revision first")?;
    let main = app
        .get_webview_window(&config.primary)
        .ok_or("primary window unavailable")?;
    let veto = evaluate(
        &main,
        &format!(
            "const f = window.{BEFORE_APPLY_GLOBAL}; if (typeof f === 'function') {{ const r = await f(); \
if (r !== undefined && r !== true) throw Error(typeof r === 'string' ? r : 'the primary window declined the reload'); }} return true;"
        ),
    )?;
    if veto["ok"] != true {
        return Err(format!(
            "apply refused: {}",
            veto["error"].as_str().unwrap_or("unknown")
        ));
    }
    let mut url = main.url().map_err(|e| e.to_string())?;
    url.set_path(&format!("{HOT_PREFIX}{revision}/index.html"));
    url.set_query(None);
    url.set_fragment(None);
    main.navigate(url).map_err(|e| e.to_string())?;
    Ok(json!({
        "primaryRevision": revision, "navigationOnly": true,
        "note": "only the primary's frontend reloaded; native code and mirror storage were not copied",
    }))
}

/// Destroy the mirror and wait until the label is really gone.
pub(crate) fn close_all(app: &AppHandle<Wry>) -> Result<(), String> {
    if let Some(w) = app.get_webview_window(LABEL) {
        w.destroy().map_err(|e| e.to_string())?;
    }
    session(|s| *s = None);
    wait_gone(app)
}

fn wait_gone(app: &AppHandle<Wry>) -> Result<(), String> {
    let until = Instant::now() + Duration::from_secs(3);
    while app.get_webview_window(LABEL).is_some() {
        if Instant::now() >= until {
            return Err("mirror window did not go away within 3s".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}

/// Revision the primary is currently showing, if it is a staged one.
fn primary_revision(app: &AppHandle<Wry>) -> Option<String> {
    let main = app.get_webview_window(&crate::config().ok()?.primary)?;
    let url = main.url().ok()?;
    let rest = url.path().strip_prefix(HOT_PREFIX)?;
    let rev = rest.split('/').next()?;
    rusty_mirror_core::protocol::is_revision(rev).then(|| rev.to_owned())
}

pub(crate) fn handle(app: &AppHandle<Wry>, request: Request) -> Result<Value, String> {
    match request {
        Request::State => state(app),
        Request::Open {
            seconds,
            width,
            height,
        } => open(app, seconds, width.zip(height)),
        Request::Eval { script } => evaluate(&mirror(app)?, &script),
        Request::Capture => capture(&mirror(app)?),
        Request::Resize { width, height } => {
            let w = mirror(app)?;
            w.set_size(tauri::LogicalSize::new(width as f64, height as f64))
                .map_err(|e| e.to_string())?;
            Ok(json!({ "size": [width, height] }))
        }
        Request::Screen { index } => {
            let w = mirror(app)?;
            if w.is_visible().unwrap_or(true) || w.is_focused().unwrap_or(true) {
                return Err("refusing to move a mirror that is visible or focused".into());
            }
            let monitors = w.available_monitors().map_err(|e| e.to_string())?;
            let m = monitors
                .get(index as usize)
                .ok_or("no monitor with that index; see `state`")?;
            let p = m.position();
            w.set_position(tauri::PhysicalPosition::new(
                p.x.saturating_add(16),
                p.y.saturating_add(16),
            ))
            .map_err(|e| e.to_string())?;
            Ok(json!({ "monitor": index, "expectedScaleFactor": m.scale_factor() }))
        }
        Request::Refresh { revision } => refresh(app, &revision),
        Request::Rollback => {
            let previous = session(|s| s.as_ref().and_then(|s| s.previous.clone())).ok_or(
                "no previous revision in this session; refresh a retained digest explicitly",
            )?;
            refresh(app, &previous)
        }
        Request::Apply => apply(app),
        Request::Close => {
            mirror(app)?;
            close_all(app)?;
            Ok(json!({ "closed": true }))
        }
    }
}
