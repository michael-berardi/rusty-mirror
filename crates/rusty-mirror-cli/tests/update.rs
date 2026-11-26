//! The real binary against a local release API: exit codes, JSON-only stdout,
//! silence without a terminal, and the update cache. No network is used.

use serde_json::{json, Value};
use std::{
    fs,
    io::{Read, Write},
    net::TcpListener,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::mpsc,
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> TestDir {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "rm-update-cli-{name}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir(&dir).unwrap();
        TestDir(dir)
    }

    fn cache(&self) -> PathBuf {
        self.0.join("cache")
    }

    fn state(&self) -> Option<Value> {
        let raw = fs::read(self.cache().join("update.json")).ok()?;
        Some(serde_json::from_slice(&raw).unwrap())
    }

    fn run(&self, api: &str, args: &[&str]) -> Output {
        self.command(api, args).output().unwrap()
    }

    fn command(&self, api: &str, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rusty-mirror"));
        command
            .args(args)
            .env("RUSTY_MIRROR_UPDATE_CACHE", self.cache())
            .env("RUSTY_MIRROR_UPDATE_API", api)
            .env("RUSTY_MIRROR_HOME", self.0.join("home"))
            .env_remove("RUSTY_MIRROR_NO_UPDATE_CHECK");
        command
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Serves the same release record to `count` requests.
fn serve(tag: &str, count: usize) -> String {
    serve_seen(tag, count).0
}

/// Like `serve`, and hands back each raw request.
fn serve_seen(tag: &str, count: usize) -> (String, mpsc::Receiver<String>) {
    let (sent, seen_requests) = mpsc::channel();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/repos/o/r/releases/latest",
        listener.local_addr().unwrap()
    );
    let body = json!({"tag_name": tag, "draft": false, "prerelease": false, "body": "- Fixes"})
        .to_string();
    thread::spawn(move || {
        for _ in 0..count {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut seen = Vec::new();
            let mut buf = [0; 4096];
            while !seen.windows(4).any(|w| w == b"\r\n\r\n") {
                match stream.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => seen.extend_from_slice(&buf[..n]),
                }
            }
            let reply = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(reply.as_bytes());
            let _ = sent.send(String::from_utf8_lossy(&seen).to_ascii_lowercase());
        }
    });
    (url, seen_requests)
}

fn closed_port() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    format!("http://{}/releases/latest", listener.local_addr().unwrap())
}

fn json_stdout(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("stdout is exactly one JSON document")
}

#[test]
fn check_exits_0_current_10_available_1_failed() {
    let dir = TestDir::new("check");
    let newer = dir.run(&serve("v99.0.0", 1), &["update", "--check"]);
    assert_eq!(newer.status.code(), Some(10));
    let report = json_stdout(&newer);
    assert_eq!(report["latest"], json!("99.0.0"));
    assert_eq!(report["updateAvailable"], json!(true));
    assert!(report["command"]
        .as_str()
        .unwrap()
        .contains("--tag v99.0.0 --locked"));
    assert!(
        newer.stderr.is_empty(),
        "no notice without a terminal: {:?}",
        newer.stderr
    );

    let current = dir.run(
        &serve(&format!("v{}", env!("CARGO_PKG_VERSION")), 1),
        &["update", "--check"],
    );
    assert_eq!(current.status.code(), Some(0));
    assert_eq!(json_stdout(&current)["updateAvailable"], json!(false));

    let failed = dir.run(&closed_port(), &["update", "--check"]);
    assert_eq!(failed.status.code(), Some(1));
    let error = json_stdout(&failed)["error"].as_str().unwrap().to_owned();
    assert!(error.starts_with("release check failed: "), "{error}");
    assert_eq!(dir.state().unwrap()["error"], json!(error));
}

#[test]
fn skip_auto_and_notify_are_remembered_and_auto_off_cleans_up() {
    let dir = TestDir::new("settings");
    let status = dir.run(&closed_port(), &["update", "--auto", "status"]);
    let report = json_stdout(&status);
    assert_eq!(
        (report["auto"].clone(), report["notify"].clone()),
        (json!(false), json!(true))
    );

    let skipped = dir.run(&serve("v99.0.0", 1), &["update", "--skip"]);
    assert_eq!(json_stdout(&skipped)["skipped"], json!("99.0.0"));
    assert_eq!(dir.state().unwrap()["skipped"], json!("99.0.0"));

    dir.run(&closed_port(), &["update", "--auto", "on"]);
    assert_eq!(dir.state().unwrap()["auto"], json!(true));
    dir.run(&closed_port(), &["update", "--notify", "off"]);
    assert_eq!(dir.state().unwrap()["notify"], json!(false));

    let off = dir.run(&closed_port(), &["update", "--auto", "off"]);
    assert_eq!(json_stdout(&off)["auto"], json!(false));
    let kept = dir.state().unwrap();
    assert_eq!(
        (kept["notify"].clone(), kept["skipped"].clone()),
        (json!(false), json!(null))
    );
    dir.run(&closed_port(), &["update", "--notify", "on"]);
    dir.run(&closed_port(), &["update", "--auto", "off"]);
    assert!(!dir.cache().exists(), "--auto off leaves no update files");

    let usage = dir.run(&closed_port(), &["update", "--check", "--skip"]);
    assert_eq!(usage.status.code(), Some(2));
    let usage = dir.run(&closed_port(), &["update", "--auto", "maybe"]);
    assert_eq!(usage.status.code(), Some(2));
}

#[test]
fn ordinary_commands_print_only_json_and_stay_silent_without_a_terminal() {
    let dir = TestDir::new("quiet");
    fs::create_dir_all(dir.cache()).unwrap();
    let stale = json!({"checked_at": 1, "latest": "99.0.0", "auto": true});
    fs::write(dir.cache().join("update.json"), stale.to_string()).unwrap();
    let empty = dir.0.join("empty");
    fs::create_dir(&empty).unwrap();
    let audit = dir.run(&closed_port(), &["audit", empty.to_str().unwrap()]);
    assert_eq!(audit.status.code(), Some(0));
    assert_eq!(json_stdout(&audit)["ok"], json!(true));
    assert!(audit.stderr.is_empty(), "no notice: {:?}", audit.stderr);
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        dir.state().unwrap(),
        stale,
        "no check was started and nothing was written"
    );
    assert_eq!(fs::read_dir(dir.cache()).unwrap().count(), 1);
}

#[test]
fn update_outside_a_cargo_install_names_the_command_and_changes_nothing() {
    let dir = TestDir::new("foreign");
    let output = dir.run(&serve("v99.0.0", 1), &["update"]);
    assert_eq!(output.status.code(), Some(1));
    let error = json_stdout(&output)["error"].as_str().unwrap().to_owned();
    assert!(
        error.contains("was not installed with cargo install"),
        "{error}"
    );
    assert!(
        error.ends_with("--tag v99.0.0 --locked --force rusty-mirror-cli"),
        "{error}"
    );
    let exe = Path::new(env!("CARGO_BIN_EXE_rusty-mirror"));
    assert!(exe.is_file());
}

#[test]
fn the_check_stays_anonymous_with_a_curl_config_file() {
    let dir = TestDir::new("curlrc");
    let leaky = "user-agent = \"leaky\"\nheader = \"X-Machine-Id: 42\"\ncookie = \"id=42\"\n";
    fs::write(dir.0.join(".curlrc"), leaky).unwrap();
    let (url, requests) = serve_seen("v99.0.0", 1);
    let output = dir
        .command(&url, &["update", "--check"])
        .env("HOME", &dir.0)
        .env("CURL_HOME", &dir.0)
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(10));
    let request = requests.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(
        request.starts_with("get /repos/o/r/releases/latest "),
        "{request}"
    );
    assert!(
        request.contains("\r\nuser-agent: rusty-mirror-update-check\r\n"),
        "{request}"
    );
    for leak in ["leaky", "x-machine-id", "cookie", env!("CARGO_PKG_VERSION")] {
        assert!(!request.contains(leak), "{leak} sent: {request}");
    }
}
