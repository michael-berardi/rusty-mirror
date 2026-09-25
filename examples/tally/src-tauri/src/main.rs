//! Tally: one number, one read (`get_tally`) and one effect (`save_tally`).
//! Everything Rusty Mirror needs is the three `mirror-debug` blocks below.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{path::PathBuf, sync::Mutex};
use tauri::{Manager, State, WebviewUrl, WebviewWindowBuilder};

struct Tally(Mutex<i64>);

fn data_file(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = match std::env::var_os("TALLY_DATA") {
        Some(dir) => PathBuf::from(dir),
        None => app.path().app_data_dir().map_err(|e| e.to_string())?,
    };
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    Ok(dir.join("tally.txt"))
}

/// Read-only: safe to allow in the mirror.
#[tauri::command]
fn get_tally(tally: State<Tally>) -> i64 {
    *tally.0.lock().unwrap()
}

/// An effect: writes to disk. The mirror must never reach this.
#[tauri::command]
fn save_tally(app: tauri::AppHandle, tally: State<Tally>, value: i64) -> Result<(), String> {
    *tally.0.lock().unwrap() = value;
    std::fs::write(data_file(&app)?, value.to_string()).map_err(|e| e.to_string())
}

fn main() {
    #[allow(unused_mut)]
    let mut context = tauri::generate_context!();
    #[cfg(feature = "mirror-debug")]
    rusty_mirror::install_hot_assets(&mut context);

    // A plain fn pointer keeps the handler's type nameable with or without the feature.
    let handler: fn(tauri::ipc::Invoke) -> bool = tauri::generate_handler![get_tally, save_tally];
    #[cfg(feature = "mirror-debug")]
    let handler = rusty_mirror::guard(handler);

    let builder = tauri::Builder::default();
    #[cfg(feature = "mirror-debug")]
    let builder = builder.plugin(rusty_mirror::Builder::new().allow(["get_tally"]).build());

    builder
        .invoke_handler(handler)
        .setup(|app| {
            let saved = data_file(app.handle())
                .ok()
                .and_then(|f| std::fs::read_to_string(f).ok())
                .and_then(|s| s.trim().parse().ok())
                .unwrap_or(0);
            app.manage(Tally(Mutex::new(saved)));
            // TALLY_HIDDEN=1 runs the whole app off-screen, for unattended tests.
            let hidden = std::env::var_os("TALLY_HIDDEN").is_some();
            #[cfg(target_os = "macos")]
            if hidden {
                app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            }
            WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
                .title("Tally")
                .inner_size(420.0, 560.0)
                .visible(!hidden)
                .focused(!hidden)
                .build()?;
            Ok(())
        })
        .build(context)
        .expect("Tally fell over. It happens to the best of us.")
        .run(|app, event| {
            // macOS activates every freshly launched app. When running unattended,
            // hide straight away so focus returns to whatever you were doing.
            #[cfg(target_os = "macos")]
            if matches!(event, tauri::RunEvent::Ready) && std::env::var_os("TALLY_HIDDEN").is_some()
            {
                let _ = app.hide();
            }
            let _ = (app, event);
        });
}
