//! End-to-end check for scripts/install.sh and `rusty-mirror update` on the copy it
//! installs: a local release directory stands in for GitHub, a temporary HOME catches
//! writes outside the tool folder, and the real CLI binary built by Cargo is the archive
//! payload. If dist/release has been built with scripts/build-release.sh, the very same
//! check runs against those signed assets too.
use std::{
    env, fs,
    io::{BufRead, BufReader},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

const VERSION: &str = env!("CARGO_PKG_VERSION");
const MARKER: &str = "# rusty-mirror installer shim";

struct TempTree(PathBuf);
impl TempTree {
    fn new() -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = env::temp_dir().join(format!(
            "rusty-mirror-install-{}-{nanos}-{serial}",
            std::process::id()
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn child(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for TempTree {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

struct Server {
    child: Child,
    base: String,
}
impl Server {
    fn new(dir: &Path) -> Self {
        let script = r#"
import http.server, os, socketserver, sys
os.chdir(sys.argv[1])
class Handler(http.server.SimpleHTTPRequestHandler):
    def log_message(self, *args): pass
class Server(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True
with Server(("127.0.0.1", 0), Handler) as httpd:
    print(httpd.server_address[1], flush=True)
    httpd.serve_forever()
"#;
        let mut child = Command::new("python3")
            .arg("-u")
            .arg("-c")
            .arg(script)
            .arg(dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("python3 is needed to serve the local release directory");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .expect("local HTTP server did not start");
        let port: u16 = line.trim().parse().expect("no port from local HTTP server");
        Self {
            child,
            base: format!("http://127.0.0.1:{port}"),
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn asset_name() -> Option<&'static str> {
    match env::consts::OS {
        "macos" => Some("rusty-mirror-darwin-universal.tar.gz"),
        "linux" => match env::consts::ARCH {
            "x86_64" => Some("rusty-mirror-linux-x86_64.tar.gz"),
            "aarch64" => Some("rusty-mirror-linux-aarch64.tar.gz"),
            _ => None,
        },
        _ => None,
    }
}

fn run(cmd: &mut Command) -> String {
    let out = cmd.output().expect("command could not be run");
    assert!(
        out.status.success(),
        "command failed: {cmd:?}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn checksum(path: &Path) -> String {
    let mut cmd = if Path::new("/sbin/sha256sum").exists()
        || Command::new("sha256sum").arg("--version").output().is_ok()
    {
        let mut c = Command::new("sha256sum");
        c.arg(path);
        c
    } else {
        let mut c = Command::new("shasum");
        c.args(["-a", "256"]).arg(path);
        c
    };
    run(&mut cmd).split_whitespace().next().unwrap().to_string()
}

fn install(installer: &Path, base: &str, home: &Path, bin: &Path) -> std::process::Output {
    Command::new("sh")
        .arg(installer)
        .env("HOME", home)
        .env("RUSTY_MIRROR_INSTALL_DIR", home.join("tool"))
        .env("RUSTY_MIRROR_BIN_DIR", bin)
        .env("RUSTY_MIRROR_RELEASE_BASE", base)
        .env_remove("RUSTY_MIRROR_VERSION")
        .stdin(Stdio::null())
        .output()
        .expect("installer could not be run")
}

fn assert_installed(bin: &Path) {
    let shim = bin.join("rusty-mirror");
    assert!(
        fs::read_to_string(&shim).unwrap().contains(MARKER),
        "shim marker missing"
    );
    let out = Command::new(&shim).arg("--version").output().unwrap();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(out.status.success(), "installed CLI does not run: {text}");
    assert!(
        text.contains(VERSION),
        "installed CLI is {text}, expected {VERSION}"
    );
}

fn check_release(
    release: &Path,
    home: &Path,
    bin: &Path,
    asset: &str,
    base: &str,
    installer: &Path,
) {
    fs::create_dir_all(bin).unwrap();
    let out = install(installer, base, home, bin);
    assert!(
        out.status.success(),
        "installer failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_installed(bin);

    // Running it again is an update, not a failure or a second copy.
    let again = install(installer, base, home, bin);
    assert!(
        again.status.success(),
        "second install failed: {}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert_installed(bin);

    let shim = bin.join("rusty-mirror");
    fs::write(&shim, "someone else's binary\n").unwrap();
    let clobber = install(installer, base, home, bin);
    assert!(
        !clobber.status.success(),
        "installer overwrote a foreign binary"
    );
    assert_eq!(
        fs::read_to_string(&shim).unwrap(),
        "someone else's binary\n"
    );

    let sums = fs::read_to_string(release.join("SHA256SUMS")).unwrap();
    let name = sums
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap();
    fs::write(
        release.join("SHA256SUMS"),
        format!("{}  {name}\n", "0".repeat(64)),
    )
    .unwrap();
    let other_home = home.with_file_name(format!(
        "{}-tampered",
        home.file_name().unwrap().to_string_lossy()
    ));
    let other_bin = bin.with_file_name(format!(
        "{}-tampered",
        bin.file_name().unwrap().to_string_lossy()
    ));
    fs::create_dir_all(&other_bin).unwrap();
    let tampered = install(installer, base, &other_home, &other_bin);
    assert!(
        !tampered.status.success(),
        "installer accepted a bad checksum"
    );
    assert!(
        String::from_utf8_lossy(&tampered.stderr).contains("checksum mismatch"),
        "unexpected checksum failure: {}",
        String::from_utf8_lossy(&tampered.stderr)
    );
    assert!(
        !other_bin.join("rusty-mirror").exists(),
        "bad archive was installed"
    );
    let _ = asset;
}

fn make_release(repo: &Path, release: &Path, asset: &str) {
    fs::create_dir_all(release).unwrap();
    let stage = release.with_file_name("stage");
    fs::create_dir_all(stage.join("rusty-mirror")).unwrap();

    // A real CLI binary, with the same layout and metadata files the release builder ships.
    fs::copy(
        env!("CARGO_BIN_EXE_rusty-mirror"),
        stage.join("rusty-mirror/rusty-mirror"),
    )
    .unwrap();
    fs::set_permissions(
        stage.join("rusty-mirror/rusty-mirror"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    for name in ["README.md", "LICENSE", "CHANGELOG.md"] {
        fs::copy(repo.join(name), stage.join("rusty-mirror").join(name)).unwrap();
    }
    fs::write(stage.join("rusty-mirror/VERSION"), format!("{VERSION}\n")).unwrap();
    run(Command::new("tar")
        .arg("-czf")
        .arg(release.join(asset))
        .arg("-C")
        .arg(&stage)
        .arg("rusty-mirror"));
    fs::copy(repo.join("scripts/install.sh"), release.join("install.sh")).unwrap();
    let sums = format!(
        "{}  {asset}\n{}  install.sh\n",
        checksum(&release.join(asset)),
        checksum(&release.join("install.sh"))
    );
    fs::write(release.join("SHA256SUMS"), sums).unwrap();
}

#[test]
fn installer_from_local_release() {
    let Some(asset) = asset_name() else {
        eprintln!("skipped: no installer asset for this platform");
        return;
    };
    let temp = TempTree::new();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let release = temp.child("release");
    make_release(&repo, &release, asset);

    let server = Server::new(&release);
    let home = temp.child("home");
    let bin = temp.child("bin");
    fs::create_dir_all(&home).unwrap();
    check_release(
        &release,
        &home,
        &bin,
        asset,
        &server.base,
        &release.join("install.sh"),
    );

    // If the signed release assets exist, run the identical installer against them.
    let built = repo.join("dist/release");
    if built.join(asset).is_file()
        && built.join("SHA256SUMS").is_file()
        && built.join("install.sh").is_file()
    {
        let built_server = Server::new(&built);
        let built_home = temp.child("built-home");
        let built_bin = temp.child("built-bin");
        fs::create_dir_all(&built_home).unwrap();
        let out = install(
            &built.join("install.sh"),
            &built_server.base,
            &built_home,
            &built_bin,
        );
        assert!(
            out.status.success(),
            "installer failed against dist/release:\n{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert_installed(&built_bin);
        eprintln!("installer: verified dist/release assets end to end");
    } else {
        eprintln!("installer: dist/release not built; verified a local synthetic release");
    }
}

#[test]
fn installer_from_local_directory_and_file_url() {
    let Some(asset) = asset_name() else {
        eprintln!("skipped: no installer asset for this platform");
        return;
    };
    let temp = TempTree::new();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let release = temp.child("release");
    make_release(&repo, &release, asset);
    let installer = release.join("install.sh");

    // A file:// URL still goes through the download path, with no HTTP server.
    let file_home = temp.child("file-home");
    let file_bin = temp.child("file-bin");
    fs::create_dir_all(&file_home).unwrap();
    let out = install(
        &installer,
        &format!("file://{}", release.display()),
        &file_home,
        &file_bin,
    );
    assert!(
        out.status.success(),
        "file:// install failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_installed(&file_bin);

    // A local directory base copies the files, and keeps the same safety checks.
    let home = temp.child("home");
    let bin = temp.child("bin");
    fs::create_dir_all(&home).unwrap();
    check_release(
        &release,
        &home,
        &bin,
        asset,
        release.to_str().unwrap(),
        &installer,
    );

    let absent = temp.child("absent").display().to_string();
    let bad = install(
        &installer,
        &absent,
        &temp.child("bad-home"),
        &temp.child("bad-bin"),
    );
    assert!(
        !bad.status.success(),
        "installer accepted a missing base directory"
    );
    assert!(
        String::from_utf8_lossy(&bad.stderr)
            .contains("must be an existing directory or an http(s)/file URL"),
        "unexpected error: {}",
        String::from_utf8_lossy(&bad.stderr)
    );
}

#[test]
fn installer_rejects_non_x86_64_linux() {
    let Some(asset) = asset_name() else {
        eprintln!("skipped: no installer asset for this platform");
        return;
    };
    let temp = TempTree::new();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let release = temp.child("release");
    make_release(&repo, &release, asset);

    // Pretend to be Linux/aarch64 without touching the host's uname.
    let stub = temp.child("stub-bin");
    fs::create_dir_all(&stub).unwrap();
    let uname = stub.join("uname");
    fs::write(
        &uname,
        "#!/bin/sh\nif [ \"$1\" = -s ]; then echo Linux; else echo aarch64; fi\n",
    )
    .unwrap();
    fs::set_permissions(&uname, fs::Permissions::from_mode(0o755)).unwrap();
    let home = temp.child("home");
    let bin = temp.child("bin");
    fs::create_dir_all(&home).unwrap();
    let out = Command::new("sh")
        .arg(release.join("install.sh"))
        .env(
            "PATH",
            format!("{}:{}", stub.display(), env::var("PATH").unwrap()),
        )
        .env("HOME", &home)
        .env("RUSTY_MIRROR_INSTALL_DIR", home.join("tool"))
        .env("RUSTY_MIRROR_BIN_DIR", &bin)
        .env("RUSTY_MIRROR_RELEASE_BASE", &release)
        .env_remove("RUSTY_MIRROR_VERSION")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "installer accepted Linux/aarch64");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("Linux release builds are x86_64 only"),
        "unexpected error: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !bin.join("rusty-mirror").exists(),
        "bad platform installed a shim"
    );
}

#[test]
fn build_release_rejects_unsafe_out() {
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let script = repo.join("scripts/build-release.sh");
    let home = env::var("HOME").unwrap();
    for bad in ["/".to_string(), ".".to_string(), home] {
        let out = Command::new("bash")
            .arg(&script)
            .arg(&bad)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!out.status.success(), "build-release accepted OUT={bad}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("OUT must resolve under dist/"),
            "unexpected error for {bad}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// A release whose CLI is a stand-in script that reports `version`.
fn make_stand_in_release(release: &Path, asset: &str, version: &str) {
    let stage = release.with_file_name(format!("stage-{version}"));
    fs::create_dir_all(stage.join("rusty-mirror")).unwrap();
    fs::create_dir_all(release).unwrap();
    let cli = stage.join("rusty-mirror/rusty-mirror");
    fs::write(&cli, format!("#!/bin/sh\necho 'rusty-mirror {version}'\n")).unwrap();
    fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
    fs::write(stage.join("rusty-mirror/VERSION"), format!("{version}\n")).unwrap();
    run(Command::new("tar")
        .arg("-czf")
        .arg(release.join(asset))
        .arg("-C")
        .arg(&stage)
        .arg("rusty-mirror"));
    let sums = format!("{}  {asset}\n", checksum(&release.join(asset)));
    fs::write(release.join("SHA256SUMS"), sums).unwrap();
}

fn releases(tool: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(tool.join("releases"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn installed_cli_updates_itself_from_a_verified_release() {
    let Some(asset) = asset_name() else {
        eprintln!("skipped: no installer asset for this platform");
        return;
    };
    let temp = TempTree::new();
    let repo = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let release = temp.child("release");
    make_release(&repo, &release, asset);
    let (home, bin) = (temp.child("home"), temp.child("bin"));
    fs::create_dir_all(&home).unwrap();
    let out = install(
        &release.join("install.sh"),
        release.to_str().unwrap(),
        &home,
        &bin,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_installed(&bin);

    // The release API and the next release, both local.
    let next = temp.child("next");
    make_stand_in_release(&next, asset, "99.0.0");
    let api = temp.child("api");
    fs::create_dir_all(&api).unwrap();
    fs::write(
        api.join("latest"),
        r#"{"tag_name":"v99.0.0","draft":false,"prerelease":false,"body":"- Fixes"}"#,
    )
    .unwrap();
    let server = Server::new(&api);
    let update = |args: &[&str], base: &Path| {
        Command::new(bin.join("rusty-mirror"))
            .args(args)
            .env("HOME", &home)
            .env("RUSTY_MIRROR_UPDATE_CACHE", temp.child("cache"))
            .env("RUSTY_MIRROR_UPDATE_API", format!("{}/latest", server.base))
            .env("RUSTY_MIRROR_RELEASE_BASE", base)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let check = update(&["update", "--check"], &next);
    assert_eq!(
        check.status.code(),
        Some(10),
        "{}",
        String::from_utf8_lossy(&check.stdout)
    );
    let report: serde_json::Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!(report["command"], "rusty-mirror update");

    // A tampered archive is refused and the installed version keeps working.
    let tampered = temp.child("tampered");
    make_stand_in_release(&tampered, asset, "99.0.0");
    fs::write(
        tampered.join("SHA256SUMS"),
        format!("{}  {asset}\n", "0".repeat(64)),
    )
    .unwrap();
    let refused = update(&["update"], &tampered);
    assert_eq!(refused.status.code(), Some(1));
    let report: serde_json::Value = serde_json::from_slice(&refused.stdout).unwrap();
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("checksum mismatch"),
        "{report}"
    );
    assert_installed(&bin);

    let done = update(&["update"], &next);
    assert!(
        done.status.success(),
        "{}",
        String::from_utf8_lossy(&done.stdout)
    );
    let report: serde_json::Value = serde_json::from_slice(&done.stdout).unwrap();
    assert_eq!(report["installed"], "99.0.0");
    let ran = Command::new(bin.join("rusty-mirror"))
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout).trim(),
        "rusty-mirror 99.0.0"
    );
    let tool = home.join("tool");
    assert_eq!(releases(&tool), [VERSION.to_string(), "99.0.0".into()]);
    let leftovers: Vec<_> = fs::read_dir(&tool)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !["releases", "current", ".installed-by-rusty-mirror"].contains(&n.as_str()))
        .collect();
    assert!(leftovers.is_empty(), "download folder left: {leftovers:?}");

    // Re-running the installer also keeps exactly one earlier release.
    let later = temp.child("later");
    make_stand_in_release(&later, asset, "99.1.0");
    fs::copy(release.join("install.sh"), later.join("install.sh")).unwrap();
    let out = install(
        &later.join("install.sh"),
        later.to_str().unwrap(),
        &home,
        &bin,
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(releases(&tool), ["99.0.0", "99.1.0"]);
}
