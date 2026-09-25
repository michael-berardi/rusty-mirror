//! # Rusty Mirror
//!
//! A hidden, disposable twin of your real Tauri window. Same renderer, same
//! frontend bundle, same state, none of the consequences. Agents drive it over
//! a private unix socket with the `rusty-mirror` CLI.
//!
//! Three lines of wiring, all behind your own `mirror-debug` feature:
//!
//! ```ignore
//! let mut context = tauri::generate_context!();
//! #[cfg(feature = "mirror-debug")]
//! rusty_mirror::install_hot_assets(&mut context);
//!
//! let handler: fn(tauri::ipc::Invoke) -> bool = tauri::generate_handler![get_state, save_state];
//! #[cfg(feature = "mirror-debug")]
//! let handler = rusty_mirror::guard(handler);
//!
//! let builder = tauri::Builder::default().invoke_handler(handler);
//! #[cfg(feature = "mirror-debug")]
//! let builder = builder.plugin(rusty_mirror::Builder::new().allow(["get_state"]).build());
//! builder.run(context).expect("tally ran off");
//! ```
//!
//! Consumer builds omit the feature, so none of this exists in them. Prove it
//! with `rusty-mirror audit`.
#![cfg_attr(not(unix), allow(unused))]

mod hot;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(unix)]
mod server;
mod window;

pub use hot::install_hot_assets;
pub use rusty_mirror_core as core;
pub use rusty_mirror_core::LABEL;

use std::sync::OnceLock;
use tauri::{
    ipc::Invoke,
    plugin::{Builder as PluginBuilder, TauriPlugin},
    Runtime, Wry,
};

/// Kept in the binary on purpose: `rusty-mirror audit` looks for it.
#[used]
static COMPILED_IN: &str = rusty_mirror_core::SEED_GLOBAL;

pub(crate) struct Config {
    pub app_id: String,
    pub primary: String,
    pub allow: Vec<String>,
    pub size: Option<(f64, f64)>,
    pub csp: String,
}

static CONFIG: OnceLock<Config> = OnceLock::new();

pub(crate) fn config() -> Result<&'static Config, String> {
    CONFIG
        .get()
        .ok_or_else(|| "rusty-mirror plugin is not registered".into())
}

/// Default mirror content security policy. Scripts and styles come from the
/// app itself; IPC is allowed (the native guard decides what it may do);
/// everything that could leave the machine is refused.
pub const DEFAULT_CSP: &str = "default-src 'self'; script-src 'self' 'unsafe-inline'; \
style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; \
connect-src 'self' ipc: http://ipc.localhost; media-src 'self' blob: data:; frame-src 'none'; \
object-src 'none'; form-action 'none'; base-uri 'self'";

/// Plugin builder. Everything is optional; the defaults are the safe ones.
#[derive(Default)]
pub struct Builder {
    app_id: Option<String>,
    primary: Option<String>,
    allow: Vec<String>,
    size: Option<(f64, f64)>,
    csp: Option<String>,
}

impl Builder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Directory name under `~/.rusty-mirror/`. Defaults to the Tauri `identifier`.
    pub fn app_id(mut self, id: impl Into<String>) -> Self {
        self.app_id = Some(id.into());
        self
    }

    /// Label of the window to mirror. Defaults to `main`.
    pub fn primary(mut self, label: impl Into<String>) -> Self {
        self.primary = Some(label.into());
        self
    }

    /// App commands the mirror may invoke. List read-only commands only:
    /// anything not listed is rejected natively when called from the mirror.
    pub fn allow<I, S>(mut self, commands: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allow.extend(commands.into_iter().map(Into::into));
        self
    }

    /// Initial mirror size in logical pixels. Defaults to the primary's size.
    pub fn size(mut self, width: f64, height: f64) -> Self {
        self.size = Some((width, height));
        self
    }

    /// Replace the mirror-only content security policy.
    pub fn csp(mut self, policy: impl Into<String>) -> Self {
        self.csp = Some(policy.into());
        self
    }

    pub fn build(self) -> TauriPlugin<Wry> {
        PluginBuilder::new("rusty-mirror")
            .setup(move |app, _api| {
                let app_id = self
                    .app_id
                    .clone()
                    .unwrap_or_else(|| app.config().identifier.clone());
                rusty_mirror_core::paths::validate_app_id(&app_id)?;
                let config = Config {
                    app_id,
                    primary: self.primary.clone().unwrap_or_else(|| "main".into()),
                    allow: self.allow.clone(),
                    size: self.size,
                    csp: self.csp.clone().unwrap_or_else(|| DEFAULT_CSP.into()),
                };
                if CONFIG.set(config).is_err() {
                    return Err("rusty-mirror registered twice".into());
                }
                #[cfg(unix)]
                if let Err(error) = server::start(app.clone()) {
                    // A mirror that cannot listen must not take the app down with it.
                    eprintln!("rusty-mirror: control socket disabled: {error}");
                }
                #[cfg(not(unix))]
                eprintln!("rusty-mirror: this platform has no control transport yet");
                Ok(())
            })
            .on_event(|_app, event| {
                if let tauri::RunEvent::Exit = event {
                    #[cfg(unix)]
                    server::cleanup();
                }
            })
            .build()
    }
}

/// True when `command` may run in the mirror window.
pub fn allowed_in_mirror(command: &str) -> bool {
    CONFIG
        .get()
        .is_some_and(|c| c.allow.iter().any(|a| a == command))
}

/// Wrap your invoke handler so the mirror window can only call allowlisted
/// commands. The check is native: a frontend that forgets to behave still
/// cannot save, send, delete or spawn anything from the mirror.
pub fn guard<R, F>(handler: F) -> impl Fn(Invoke<R>) -> bool + Send + Sync + 'static
where
    R: Runtime,
    F: Fn(Invoke<R>) -> bool + Send + Sync + 'static,
{
    move |invoke: Invoke<R>| {
        if invoke.message.webview_ref().label() == LABEL {
            let command = invoke.message.command().to_owned();
            if !allowed_in_mirror(&command) {
                invoke.resolver.reject(format!(
                    "`{command}` is not allowed in Rusty Mirror: effects are denied in the mirror. \
                     Allow read-only commands with rusty_mirror::Builder::allow."
                ));
                return true;
            }
        }
        handler(invoke)
    }
}

/// True inside the mirror window. Handy in commands that want to simulate.
pub fn is_mirror<R: Runtime>(webview: &tauri::Webview<R>) -> bool {
    webview.label() == LABEL
}
