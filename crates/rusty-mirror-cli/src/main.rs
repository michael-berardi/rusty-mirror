//! `rusty-mirror`: the agent-facing end of the mirror. Every command prints one
//! JSON document on stdout. Exit codes: 0 ok, 1 failed, 2 usage, 3 audit found
//! mirror code where it must not be.
use base64::Engine;
use clap::{Parser, Subcommand};
use rusty_mirror_core::{audit, paths, protocol, protocol::Request, stage};
use serde_json::{json, Value};
use std::{
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

#[derive(Parser)]
#[command(
    name = "rusty-mirror",
    version,
    about = "Drive a hidden, effect-denied twin of your real Tauri app.",
    after_help = "Typical loop:\n  rusty-mirror open\n  rusty-mirror eval 'return document.title'\n  \
rusty-mirror capture shots/before.png\n  rusty-mirror stage dist && rusty-mirror refresh <rev>\n  rusty-mirror close\n\n\
Or let run own the lifecycle:  rusty-mirror run -- ./qa.sh"
)]
struct Cli {
    /// App id (the Tauri identifier). Defaults to $RUSTY_MIRROR_APP, or the only running app.
    #[arg(long, global = true, env = "RUSTY_MIRROR_APP")]
    app: Option<String>,
    /// Socket override (normally derived from the app id).
    #[arg(long, global = true, env = "RUSTY_MIRROR_SOCKET")]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Is a mirror open, where, at what size and revision?
    State,
    /// Snapshot the primary window and open an invisible mirror.
    Open {
        /// Lifetime backstop in seconds (1-3600). Close explicitly anyway.
        #[arg(long, default_value_t = 900)]
        seconds: u64,
        /// Logical size, e.g. 1920x1080. Defaults to the primary's size.
        #[arg(long, value_parser = parse_size)]
        size: Option<(u32, u32)>,
    },
    /// Run an async JS function body in the mirror; `return` a JSON value.
    Eval {
        /// Script body, or `-` to read stdin.
        script: Option<String>,
        /// Read the script body from a file.
        #[arg(short, long, conflicts_with = "script")]
        file: Option<PathBuf>,
    },
    /// Save a native renderer snapshot of the hidden mirror as PNG.
    Capture {
        /// Output path. Never overwritten.
        out: PathBuf,
    },
    /// Resize the mirror, e.g. 1440x900.
    Resize {
        #[arg(value_parser = parse_size)]
        size: (u32, u32),
    },
    /// Move the invisible mirror to monitor N (see `state`) to test scale factors.
    Screen { index: u32 },
    /// Copy a built frontend into an immutable, verified revision. Offline.
    Stage { dist: PathBuf },
    /// Rebuild the mirror on a staged revision, keeping its local state.
    Refresh { revision: String },
    /// Refresh the mirror back to its previous revision.
    Rollback,
    /// Reload the PRIMARY window on the mirror's verified revision.
    Apply,
    /// Close the mirror. Do this every session.
    Close,
    /// Open a mirror, run a command, and always close it afterwards.
    Run {
        #[arg(long, default_value_t = 900)]
        seconds: u64,
        #[arg(long, value_parser = parse_size)]
        size: Option<(u32, u32)>,
        #[arg(trailing_var_arg = true, required = true)]
        command: Vec<String>,
    },
    /// Prove a build does not contain the mirror (dist dirs, binaries, .app bundles).
    Audit {
        /// Files or directories that must be mirror-free.
        paths: Vec<PathBuf>,
        /// Tauri capabilities directory that must not grant the mirror window anything.
        #[arg(long)]
        capabilities: Option<PathBuf>,
    },
    /// Check the local setup and list apps with a live control socket.
    Doctor,
}

fn parse_size(s: &str) -> Result<(u32, u32), String> {
    let (w, h) = s.split_once('x').ok_or("expected WIDTHxHEIGHT")?;
    let size = (
        w.parse().map_err(|_| "bad width")?,
        h.parse().map_err(|_| "bad height")?,
    );
    protocol::check_size(size.0, size.1)?;
    Ok(size)
}

fn print(value: &Value) {
    println!("{}", serde_json::to_string_pretty(value).unwrap());
}

fn fail(error: impl std::fmt::Display) -> ExitCode {
    print(&json!({"ok": false, "error": error.to_string()}));
    ExitCode::from(1)
}

struct Target {
    app: String,
    socket: PathBuf,
}

fn target(cli: &Cli) -> Result<Target, String> {
    let app = match &cli.app {
        Some(app) => app.clone(),
        None => match paths::known_apps().as_slice() {
            [one] => one.clone(),
            [] => {
                return Err("no running mirror-enabled app found; pass --app <identifier>".into())
            }
            many => {
                return Err(format!(
                    "several apps are running ({}); pass --app",
                    many.join(", ")
                ))
            }
        },
    };
    paths::validate_app_id(&app)?;
    let socket = match &cli.socket {
        Some(s) => s.clone(),
        None => paths::socket_path(&app)?,
    };
    Ok(Target { app, socket })
}

fn call(t: &Target, request: Request) -> Result<Value, String> {
    let timeout = Duration::from_secs(if matches!(request, Request::Capture) {
        30
    } else {
        20
    });
    let response = protocol::call(&t.socket, &request, timeout)?;
    if response["ok"] == true {
        Ok(response["result"].clone())
    } else {
        Err(response["error"]
            .as_str()
            .unwrap_or("request failed")
            .to_owned())
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(error) => fail(error),
    }
}

fn run(cli: &Cli) -> Result<ExitCode, String> {
    match &cli.command {
        Command::Audit {
            paths,
            capabilities,
        } => return Ok(audit_cmd(paths, capabilities.as_deref())),
        Command::Doctor => return Ok(doctor(cli)),
        Command::Stage { dist } => {
            let t = target_for_stage(cli)?;
            let s = stage::stage(&t, dist)?;
            print(
                &json!({"ok": true, "app": t, "revision": s.revision, "path": s.path, "files": s.files,
                "bytes": s.bytes, "alreadyStaged": s.already_staged, "primaryUntouched": true,
                "next": format!("rusty-mirror refresh {}", s.revision)}),
            );
            return Ok(ExitCode::SUCCESS);
        }
        _ => {}
    }
    let t = target(cli)?;
    let request = match &cli.command {
        Command::State => Request::State,
        Command::Open { seconds, size } => open_request(*seconds, *size),
        Command::Eval { script, file } => {
            let script = match (script.as_deref(), file) {
                (_, Some(f)) => {
                    std::fs::read_to_string(f).map_err(|e| format!("{}: {e}", f.display()))?
                }
                (Some("-"), _) => {
                    std::io::read_to_string(std::io::stdin()).map_err(|e| e.to_string())?
                }
                (Some(s), _) => s.to_owned(),
                (None, None) => {
                    return Err("eval needs a script body, -f FILE, or - for stdin".into())
                }
            };
            let result = call(&t, Request::Eval { script })?;
            // The transport worked; the script's own verdict decides the exit code.
            let ok = result["ok"] == true;
            print(&json!({"ok": ok, "value": result["value"], "error": result["error"]}));
            return Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            });
        }
        Command::Capture { out } => return capture(&t, out),
        Command::Resize { size } => Request::Resize {
            width: size.0,
            height: size.1,
        },
        Command::Screen { index } => Request::Screen { index: *index },
        Command::Refresh { revision } => Request::Refresh {
            revision: revision.clone(),
        },
        Command::Rollback => Request::Rollback,
        Command::Apply => Request::Apply,
        Command::Close => Request::Close,
        Command::Run {
            seconds,
            size,
            command,
        } => return run_session(&t, *seconds, *size, command),
        Command::Audit { .. } | Command::Doctor | Command::Stage { .. } => unreachable!(),
    };
    let result = call(&t, request)?;
    print(&json!({"ok": true, "result": result}));
    Ok(ExitCode::SUCCESS)
}

fn target_for_stage(cli: &Cli) -> Result<String, String> {
    // Staging is offline, so the app need not be running; an explicit id is fine.
    match &cli.app {
        Some(app) => {
            paths::validate_app_id(app)?;
            Ok(app.clone())
        }
        None => target(cli).map(|t| t.app),
    }
}

fn open_request(seconds: u64, size: Option<(u32, u32)>) -> Request {
    Request::Open {
        seconds,
        width: size.map(|s| s.0),
        height: size.map(|s| s.1),
    }
}

fn capture(t: &Target, out: &Path) -> Result<ExitCode, String> {
    if out.extension().and_then(|e| e.to_str()) != Some("png") {
        return Err("capture path must end in .png".into());
    }
    let result = call(t, Request::Capture)?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(result["png"].as_str().ok_or("capture returned no image")?)
        .map_err(|e| e.to_string())?;
    let stats = analyse(&png)?;
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(out)
        .map_err(|e| format!("{}: {e} (captures are never overwritten)", out.display()))?;
    file.write_all(&png).map_err(|e| e.to_string())?;
    let blank = stats["looksBlank"] == true;
    print(&json!({
        "ok": !blank, "path": out, "bytes": png.len(), "renderer": result["renderer"],
        "mirrorVisible": result["visible"], "image": stats,
        "error": blank.then_some("capture is (nearly) one flat colour; treat as failure, not evidence"),
        "reminder": "a written file is not a passed check: look at the image",
    }));
    Ok(if blank {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

/// Size plus a cheap blankness check: a solid rectangle is not a screenshot.
fn analyse(bytes: &[u8]) -> Result<Value, String> {
    let mut decoder = png::Decoder::new(bytes);
    decoder.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = decoder.read_info().map_err(|e| format!("bad PNG: {e}"))?;
    let mut buf = vec![0; reader.output_buffer_size()];
    let info = reader
        .next_frame(&mut buf)
        .map_err(|e| format!("bad PNG: {e}"))?;
    let channels = info.color_type.samples();
    let pixels = buf[..info.buffer_size()].chunks_exact(channels);
    let total = pixels.len().max(1);
    let mut counts = std::collections::HashMap::<&[u8], usize>::new();
    for (i, px) in pixels.enumerate() {
        if i % 7 == 0 {
            *counts.entry(px).or_default() += 1;
        }
    }
    let sampled: usize = counts.values().sum();
    let dominant = counts.values().copied().max().unwrap_or(0) as f64 / sampled.max(1) as f64;
    Ok(json!({
        "width": info.width, "height": info.height, "pixels": total,
        "distinctColoursSampled": counts.len(), "dominantColourShare": (dominant * 1000.0).round() / 1000.0,
        "looksBlank": counts.len() <= 2 || dominant > 0.995,
    }))
}

fn run_session(
    t: &Target,
    seconds: u64,
    size: Option<(u32, u32)>,
    command: &[String],
) -> Result<ExitCode, String> {
    let opened = call(t, open_request(seconds, size))?;
    eprintln!(
        "rusty-mirror: opened ({}s backstop); running {:?}",
        seconds,
        command.join(" ")
    );
    // Ignore Ctrl-C/TERM here: the child receives them, then we still close.
    let previous = unsafe {
        (
            libc::signal(libc::SIGINT, libc::SIG_IGN),
            libc::signal(libc::SIGTERM, libc::SIG_IGN),
            libc::signal(libc::SIGHUP, libc::SIG_IGN),
        )
    };
    let mut child = std::process::Command::new(&command[0]);
    // SIG_IGN survives exec; give the child normal signal handling back.
    unsafe {
        use std::os::unix::process::CommandExt;
        child.pre_exec(|| {
            for signal in [libc::SIGINT, libc::SIGTERM, libc::SIGHUP] {
                libc::signal(signal, libc::SIG_DFL);
            }
            Ok(())
        });
    }
    let status = child
        .args(&command[1..])
        .env("RUSTY_MIRROR_APP", &t.app)
        .env("RUSTY_MIRROR_SOCKET", &t.socket)
        .status();
    let closed = call(t, Request::Close);
    unsafe {
        libc::signal(libc::SIGINT, previous.0);
        libc::signal(libc::SIGTERM, previous.1);
        libc::signal(libc::SIGHUP, previous.2);
    }
    let still_open = call(t, Request::State)
        .map(|s| s["open"] == true)
        .unwrap_or(false);
    let code = status.as_ref().ok().and_then(|s| s.code()).unwrap_or(1);
    let ok = status.as_ref().is_ok_and(|s| s.success()) && closed.is_ok() && !still_open;
    print(&json!({
        "ok": ok, "opened": opened, "exitCode": code,
        "spawnError": status.as_ref().err().map(|e| e.to_string()),
        "closed": closed.is_ok(), "closeError": closed.err(), "stillOpen": still_open,
    }));
    Ok(if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(code.clamp(1, 255) as u8)
    })
}

fn audit_cmd(targets: &[PathBuf], capabilities: Option<&Path>) -> ExitCode {
    if targets.is_empty() && capabilities.is_none() {
        print(
            &json!({"ok": false, "error": "audit needs at least one path or --capabilities DIR"}),
        );
        return ExitCode::from(2);
    }
    let mut findings = Vec::new();
    let mut scanned = 0;
    let mut errors = Vec::new();
    for path in targets {
        match audit::scan(path) {
            Ok((f, n)) => {
                findings.extend(f);
                scanned += n;
            }
            Err(e) => errors.push(e),
        }
    }
    let mut capability_files = 0;
    if let Some(dir) = capabilities {
        match audit::capabilities(dir) {
            Ok((f, n)) => {
                findings.extend(f);
                capability_files = n;
            }
            Err(e) => errors.push(e),
        }
    }
    let clean = findings.is_empty() && errors.is_empty();
    print(&json!({
        "ok": clean, "filesScanned": scanned, "capabilityFiles": capability_files,
        "findings": findings, "errors": errors,
        "verdict": if clean { "clean: no mirror code or mirror permissions found" }
                   else if errors.is_empty() { "NOT clean: this must not ship to users" }
                   else { "incomplete: some paths could not be read" },
    }));
    if !errors.is_empty() {
        ExitCode::from(1)
    } else if clean {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(3)
    }
}

fn doctor(cli: &Cli) -> ExitCode {
    let root = paths::root();
    let apps = paths::known_apps();
    let mut report = json!({
        "ok": true,
        "version": env!("CARGO_PKG_VERSION"),
        "platform": std::env::consts::OS,
        "nativeCapture": if cfg!(target_os = "macos") { "WKWebView.takeSnapshot" } else { "not yet on this platform; eval works" },
        "root": root.as_ref().map(|p| p.display().to_string()).unwrap_or_else(|e| e.clone()),
        "rootPrivate": root.as_ref().map(|p| !p.exists() || paths::check_private_dir(p).is_ok()).unwrap_or(false),
        "appsWithSockets": apps,
    });
    if let Ok(t) = target(cli) {
        report["app"] = json!(t.app);
        report["socket"] = json!(t.socket);
        match call(&t, Request::State) {
            Ok(state) => report["state"] = state,
            Err(e) => {
                report["ok"] = json!(false);
                report["error"] = json!(e);
            }
        }
        let staged = paths::hot_dir(&t.app)
            .ok()
            .and_then(|d| std::fs::read_dir(d).ok())
            .map(|it| {
                it.filter_map(Result::ok)
                    .filter(|e| protocol::is_revision(&e.file_name().to_string_lossy()))
                    .count()
            })
            .unwrap_or(0);
        report["stagedRevisions"] = json!(staged);
    }
    let ok = report["ok"] == true;
    print(&report);
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}
