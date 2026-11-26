//! Update notices and opt-in self-update (update contract v1, docs/updates.md).
//!
//! Cargo installs are updated with `cargo install --git … --tag vX --locked`.
//! Script installs are updated by downloading `install.sh`, `SHA256SUMS` and
//! the platform archive into a private build folder, verifying both checksums
//! and running the installer against that folder. The only automatic network
//! request is one anonymous GET of the public latest-release record, at most
//! once a day, from a detached helper. State is one small 0600 file in the
//! platform cache directory.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    env,
    fmt::Write as FmtWrite,
    fs,
    io::{self, IsTerminal, Write},
    os::unix::fs::{symlink, DirBuilderExt, OpenOptionsExt},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, ExitCode, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

const TOOL: &str = "rusty-mirror";
const PACKAGE: &str = env!("CARGO_PKG_NAME");
const CURRENT: &str = env!("CARGO_PKG_VERSION");
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const USER_AGENT: &str = "rusty-mirror-update-check";
const DAY: u64 = 24 * 60 * 60;
const LOG_LIMIT: usize = 256 * 1024;
const BUILD_PREFIX: &str = "rusty-mirror-update-";
const INSTALLER_MARKER: &str = ".installed-by-rusty-mirror";
const SHIM_MARKER: &str = "# rusty-mirror installer shim";
const RELEASE_BASE_VAR: &str = "RUSTY_MIRROR_UPDATE_RELEASE_BASE";

/// Everything the updater remembers. Missing fields take their defaults.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct State {
    /// Unix time of the last check, successful or not.
    checked_at: Option<u64>,
    /// Newest stable release seen (`x.y.z`) and the start of its notes.
    latest: Option<String>,
    notes: Option<String>,
    /// Cause of the last failed check.
    error: Option<String>,
    /// Version whose notice is hidden until a newer one is released.
    skipped: Option<String>,
    /// Last notice shown, for the once-a-day-per-version limit.
    shown: Option<String>,
    shown_at: Option<u64>,
    /// Daily check and notice (`update --notify`). On by default.
    notify: bool,
    /// Automatic installs (`update --auto`). Off by default.
    auto: bool,
    /// The `[y/N]` question after an install is asked only once.
    auto_asked: bool,
    /// Version a background install last tried, and why it failed.
    auto_tried: Option<String>,
    auto_error: Option<String>,
    /// Version that ran before the last install, for `update --rollback`.
    previous: Option<String>,
}

impl Default for State {
    fn default() -> Self {
        State {
            checked_at: None,
            latest: None,
            notes: None,
            error: None,
            skipped: None,
            shown: None,
            shown_at: None,
            notify: true,
            auto: false,
            auto_asked: false,
            auto_tried: None,
            auto_error: None,
            previous: None,
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The update cache directory: `update.json` and the auto-install `update.log`.
struct Store {
    dir: PathBuf,
}

impl Store {
    /// `$RUSTY_MIRROR_UPDATE_CACHE`, else the platform cache directory.
    fn locate() -> Result<Store, String> {
        let var = |key: &str| {
            env::var_os(key)
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
        };
        let dir = match var("RUSTY_MIRROR_UPDATE_CACHE") {
            Some(dir) => dir,
            None => {
                let home = var("HOME");
                let base = if cfg!(target_os = "macos") {
                    home.map(|home| home.join("Library/Caches"))
                } else {
                    var("XDG_CACHE_HOME").or_else(|| home.map(|home| home.join(".cache")))
                };
                base.ok_or("HOME is not set")?.join(TOOL)
            }
        };
        if !dir.is_absolute() {
            return Err(format!("update cache {} is not absolute", dir.display()));
        }
        Ok(Store { dir })
    }

    fn state_path(&self) -> PathBuf {
        self.dir.join("update.json")
    }

    fn log_path(&self) -> PathBuf {
        self.dir.join("update.log")
    }

    fn load(&self) -> State {
        fs::read(self.state_path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self, state: &State) -> Result<(), String> {
        let bytes = serde_json::to_vec_pretty(state).map_err(|e| e.to_string())?;
        write_private(&self.state_path(), &bytes)
    }

    /// Append a line, keeping the log under 256 KiB by dropping its oldest half.
    fn log(&self, line: &str) {
        let path = self.log_path();
        let mut bytes = fs::read(&path).unwrap_or_default();
        bytes.extend_from_slice(format!("{} {line}\n", now()).as_bytes());
        if bytes.len() > LOG_LIMIT {
            let cut = bytes.len() - LOG_LIMIT / 2;
            let start = bytes[cut..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(bytes.len(), |i| cut + i + 1);
            bytes.drain(..start);
        }
        let _ = write_private(&path, &bytes);
    }

    /// `update --auto off`: delete the cache and the log. A file stays only to
    /// remember `--notify off`.
    fn forget(&self, state: &State) -> Result<(), String> {
        let _ = fs::remove_file(self.log_path());
        if !state.notify {
            return self.save(&State {
                notify: false,
                ..State::default()
            });
        }
        match fs::remove_file(self.state_path()) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => {
                return Err(format!("{}: {e}", self.state_path().display()))
            }
            _ => {}
        }
        let _ = fs::remove_dir(&self.dir); // only succeeds when nothing else is in it
        Ok(())
    }
}

/// Atomic private write: a 0700 directory, a 0600 temp file, one rename.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let fail = |e: io::Error| format!("{}: {e}", path.display());
    let dir = path.parent().ok_or("update cache has no directory")?;
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(fail)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let tmp = dir.join(format!(".{name}.{}", std::process::id()));
    let _ = fs::remove_file(&tmp);
    let written = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&tmp)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| fs::rename(&tmp, path));
    if written.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    written.map_err(fail)
}

/// Stable `x.y.z`, optionally `v`-prefixed. Anything else is not a version.
fn parse_version(raw: &str) -> Option<(u64, u64, u64)> {
    let mut parts = raw.strip_prefix('v').unwrap_or(raw).split('.');
    let mut next = || {
        let part = parts.next()?;
        let digits = !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit());
        (digits && (part == "0" || !part.starts_with('0')))
            .then(|| part.parse().ok())
            .flatten()
    };
    let version = (next()?, next()?, next()?);
    parts.next().is_none().then_some(version)
}

fn is_newer(candidate: &str, current: &str) -> bool {
    matches!(
        (parse_version(candidate), parse_version(current)),
        (Some(new), Some(old)) if new > old
    )
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    body: Option<String>,
}

/// The release as `(x.y.z, short notes)`; drafts, prereleases and odd tags are `None`.
fn parse_release(raw: &str) -> Result<Option<(String, String)>, String> {
    let release: Release =
        serde_json::from_str(raw).map_err(|e| format!("unreadable release record: {e}"))?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    Ok(
        parse_version(&release.tag_name).map(|(major, minor, patch)| {
            (
                format!("{major}.{minor}.{patch}"),
                short_notes(release.body.as_deref().unwrap_or("")),
            )
        }),
    )
}

/// The first five lines of the release notes as plain text.
fn short_notes(body: &str) -> String {
    body.lines()
        .map(|line| line.trim().trim_start_matches(['#', '-', '*', '>']).trim())
        .filter(|line| !line.is_empty())
        .take(5)
        .map(|line| line.chars().take(100).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

fn api_url() -> String {
    env::var("RUSTY_MIRROR_UPDATE_API")
        .ok()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| {
            let slug = REPOSITORY.trim_start_matches("https://github.com/");
            format!("https://api.github.com/repos/{slug}/releases/latest")
        })
}

/// One anonymous GET: fixed User-Agent, no `~/.curlrc`, no cookies, 4 s at most.
fn fetch(url: &str) -> Result<String, String> {
    let output = Command::new("curl")
        .args([
            "--disable",
            "--silent",
            "--show-error",
            "--fail",
            "--location",
        ])
        .args([
            "--max-redirs",
            "3",
            "--max-time",
            "4",
            "--max-filesize",
            "1000000",
        ])
        .args(["--user-agent", USER_AGENT])
        .args(["--header", "Accept: application/vnd.github+json", url])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    if !output.status.success() {
        return Err(format!("release check failed: {}", tail(&output.stderr)));
    }
    String::from_utf8(output.stdout).map_err(|_| "release record is not UTF-8".into())
}

/// The last lines of a tool's stderr, short enough for a JSON error.
fn tail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let lines: Vec<&str> = text.lines().filter(|l| !l.trim().is_empty()).collect();
    let tail = lines[lines.len().saturating_sub(3)..].join(" | ");
    tail.chars().take(400).collect()
}

/// Ask for the latest release now and record the result or the failure's cause.
fn check(store: &Store, url: &str) -> Result<State, String> {
    let fetched = fetch(url).and_then(|raw| parse_release(&raw));
    let mut state = store.load();
    state.checked_at = Some(now());
    match &fetched {
        Ok(found) => {
            state.error = None;
            (state.latest, state.notes) = found.clone().unzip();
        }
        Err(cause) => state.error = Some(cause.clone()),
    }
    store.save(&state)?;
    fetched.map(|_| state)
}

/// A clock set backwards does not stall checks or notices.
fn due(at: Option<u64>, now: u64) -> bool {
    at.map_or(true, |at| at > now || now - at >= DAY)
}

fn available<'a>(state: &'a State, current: &str) -> Option<&'a str> {
    state
        .latest
        .as_deref()
        .filter(|latest| is_newer(latest, current))
}

/// Automatic checks and notices are for a person at a terminal: never in CI,
/// agent shells or after an opt-out. Explicit `update` commands are not gated.
fn automatic(terminal: bool, state: &State, var: impl Fn(&str) -> Option<String>) -> bool {
    let set = |key: &str| {
        var(key).is_some_and(|value| {
            !matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "" | "0" | "false" | "no" | "off"
            )
        })
    };
    let off = [
        "RUSTY_MIRROR_NO_UPDATE_CHECK",
        "CI",
        "CLAUDECODE",
        "CODEX_SANDBOX",
        "GEMINI_CLI",
    ];
    terminal && state.notify && !off.into_iter().any(set)
}

fn notice(state: &State, current: &str, now: u64) -> Option<[String; 2]> {
    let new = available(state, current)?;
    let shown_today = state.shown.as_deref() == Some(new) && !due(state.shown_at, now);
    if state.skipped.as_deref() == Some(new) || shown_today {
        return None;
    }
    let skip = format!("skip: {TOOL} update --skip");
    Some([
        format!("{TOOL} {new} available (you have {current}) · run: {TOOL} update"),
        if state.auto {
            skip
        } else {
            format!("turn on automatic updates: {TOOL} update --auto on · {skip}")
        },
    ])
}

fn write_notice(out: &mut impl Write, lines: &[String; 2], color: bool) -> io::Result<()> {
    let (dim, reset) = if color {
        ("\x1b[2m", "\x1b[0m")
    } else {
        ("", "")
    };
    write!(out, "{}\n{dim}{}{reset}\n", lines[0], lines[1])
}

fn auto_pending(state: &State, current: &str) -> bool {
    state.auto
        && available(state, current).is_some_and(|new| {
            state.skipped.as_deref() != Some(new) && state.auto_tried.as_deref() != Some(new)
        })
}

/// After an ordinary command: at most one notice on stderr, then at most one
/// detached helper. Nothing here waits for the network.
pub(crate) fn after_command() {
    let terminal = io::stdout().is_terminal() && io::stderr().is_terminal();
    if !terminal {
        return;
    }
    let Ok(store) = Store::locate() else { return };
    let mut state = store.load();
    if !automatic(terminal, &state, |key| env::var(key).ok()) {
        return;
    }
    let now = now();
    if let Some(lines) = notice(&state, CURRENT, now) {
        let color =
            env::var_os("NO_COLOR").is_none() && env::var("TERM").map_or(true, |t| t != "dumb");
        if write_notice(&mut io::stderr(), &lines, color).is_ok() {
            state.shown = state.latest.clone();
            state.shown_at = Some(now);
            let _ = store.save(&state);
        }
    }
    let check = due(state.checked_at, now);
    if !check && !auto_pending(&state, CURRENT) {
        return;
    }
    if check {
        // Claim today's check first, so commands run side by side make one request.
        state.checked_at = Some(now);
        let _ = store.save(&state);
    }
    let mut helper = Command::new(env::current_exe().unwrap_or_else(|_| TOOL.into()));
    helper.arg(crate::UPDATE_HELPER);
    if check {
        helper.arg("--check");
    }
    let spawned = helper
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    if let Err(e) = spawned {
        state.error = Some(format!("cannot start the update check: {e}"));
        let _ = store.save(&state);
    }
}

/// The detached helper: the daily check when asked, then an opted-in install.
pub(crate) fn background(check_first: bool) {
    let Ok(store) = Store::locate() else { return };
    if check_first && check(&store, &api_url()).is_err() {
        return; // the cause is in the cache
    }
    if auto_pending(&store.load(), CURRENT) {
        // SAFETY: setpriority only lowers the priority of this helper process.
        unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, 10) };
        auto_install(&store, Installer::locate(), CURRENT);
    }
}

/// One background attempt per version. A failure keeps the current version,
/// records its cause and leaves the notice to offer `update` again.
fn auto_install(store: &Store, installer: Result<Installer, String>, current: &str) {
    let mut state = store.load();
    if !auto_pending(&state, current) {
        return;
    }
    let new = state.latest.clone().unwrap_or_default();
    state.auto_tried = Some(new.clone());
    state.auto_error = None;
    if store.save(&state).is_err() {
        return; // no record, no attempt: never a retry loop
    }
    store.log(&format!("installing {new}"));
    let result = installer.and_then(|installer| installer.install(&new, current, false));
    let mut state = store.load();
    match result {
        Ok(()) => {
            state.previous = Some(current.into());
            store.log(&format!("installed {new}; it runs from the next command"));
        }
        Err(cause) => {
            store.log(&format!("install of {new} failed: {cause}"));
            state.auto_error = Some(cause);
        }
    }
    let _ = store.save(&state);
}

/// The `cargo install` root that owns `exe`, if Cargo installed it.
fn cargo_root(exe: &Path) -> Option<PathBuf> {
    let exe = fs::canonicalize(exe).unwrap_or_else(|_| exe.to_path_buf());
    let bin = exe
        .parent()
        .filter(|bin| bin.file_name() == Some("bin".as_ref()))?;
    let root = bin.parent()?;
    let installs = fs::read_to_string(root.join(".crates2.json")).ok()?;
    installs
        .contains(&format!("\"{PACKAGE} "))
        .then(|| root.to_path_buf())
}

/// Who owns the running binary, and therefore how an update must be installed.
#[derive(Clone, Debug, PartialEq)]
enum InstallKind {
    /// Cargo owns the binary in `<root>/bin`, recorded in `.crates2.json`.
    Cargo { root: PathBuf, cargo: PathBuf },
    /// scripts/install.sh owns `home/releases/<version>` and the shim in `bin`.
    Installer { home: PathBuf, bin: PathBuf },
}

impl InstallKind {
    /// Detect the owner from a (canonicalized) path to the running binary.
    fn locate(exe: &Path) -> Option<InstallKind> {
        installer_home(exe).or_else(|| {
            cargo_root(exe).map(|root| {
                let rustup = root.join("bin").join("cargo");
                InstallKind::Cargo {
                    cargo: if rustup.is_file() {
                        rustup
                    } else {
                        "cargo".into()
                    },
                    root,
                }
            })
        })
    }
}

/// The installer home that owns `exe`, when it is the active script install.
/// An explicit `bin` lets tests describe a fixture without touching the environment.
fn installer_home_from(exe: &Path, bin: Option<&Path>) -> Option<InstallKind> {
    let exe = fs::canonicalize(exe).ok()?;
    let release = exe.parent()?;
    let releases = release.parent()?;
    if releases.file_name() != Some("releases".as_ref()) {
        return None;
    }
    parse_version(release.file_name()?.to_str()?)?;
    let home = releases.parent()?;
    if !home.join(INSTALLER_MARKER).is_file() {
        return None; // never take over a folder we did not create
    }
    if fs::canonicalize(home.join("current")).ok()? != release {
        return None; // only the release currently in use may be updated
    }
    let bin = bin
        .map(Path::to_path_buf)
        .unwrap_or_else(|| installer_bin_dir(home));
    Some(InstallKind::Installer {
        home: home.to_path_buf(),
        bin,
    })
}

fn installer_home(exe: &Path) -> Option<InstallKind> {
    installer_home_from(exe, None)
}

/// A shim created by scripts/install.sh for this home.
fn marked_shim(dir: &Path, home: &Path) -> bool {
    let text = fs::read_to_string(dir.join(TOOL)).unwrap_or_default();
    text.contains(SHIM_MARKER) && text.contains(&format!("{}/current/rusty-mirror", home.display()))
}

/// The shim's folder: an explicit override, a marked shim on PATH, or the default.
fn installer_bin_dir(home: &Path) -> PathBuf {
    let absolute = |dir: PathBuf| {
        if dir.is_absolute() {
            dir
        } else {
            env::current_dir().unwrap_or_default().join(dir)
        }
    };
    if let Some(dir) = env::var_os("RUSTY_MIRROR_BIN_DIR").filter(|v| !v.is_empty()) {
        return absolute(PathBuf::from(dir));
    }
    let user_home = env::var_os("HOME").map(PathBuf::from);
    if let Some(dir) = user_home.as_ref().map(|home| home.join(".local/bin")) {
        if marked_shim(&dir, home) {
            return dir;
        }
    }
    if let Some(path) = env::var_os("PATH") {
        for dir in env::split_paths(&path) {
            if marked_shim(&dir, home) {
                return dir;
            }
        }
    }
    user_home
        .map(|home| home.join(".local/bin"))
        .unwrap_or_else(|| home.join("bin"))
}

fn github_slug() -> &'static str {
    REPOSITORY.trim_start_matches("https://github.com/")
}

/// The release directory or URL prefix, overridable for tests.
fn release_base(version: &str, override_base: Option<&str>) -> String {
    let chosen = override_base
        .map(str::to_owned)
        .or_else(|| env::var(RELEASE_BASE_VAR).ok().filter(|v| !v.is_empty()))
        .unwrap_or_else(|| {
            format!(
                "https://github.com/{}/releases/download/v{version}/",
                github_slug()
            )
        });
    chosen.trim_end_matches('/').to_owned()
}

/// The archive built for this platform by scripts/build-release.sh.
fn platform_asset() -> Result<&'static str, String> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", _) => Ok("rusty-mirror-darwin-universal.tar.gz"),
        ("linux", "x86_64") => Ok("rusty-mirror-linux-x86_64.tar.gz"),
        (os, arch) => Err(format!("no installer update asset for {os} {arch}")),
    }
}

/// One fixed User-Agent for every request this updater makes.
fn download_asset(base: &str, name: &str, dest: &Path) -> Result<(), String> {
    if base.starts_with("http://") || base.starts_with("https://") || base.starts_with("file://") {
        let output = Command::new("curl")
            .args([
                "--disable",
                "--silent",
                "--show-error",
                "--fail",
                "--location",
                "--max-redirs",
                "3",
                "--max-time",
                "60",
                "--user-agent",
                USER_AGENT,
                "--output",
            ])
            .arg(dest)
            .arg(format!("{base}/{name}"))
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot download {name}: {e}"))?;
        return output
            .status
            .success()
            .then_some(())
            .ok_or_else(|| format!("cannot download {name}: {}", tail(&output.stderr)));
    }
    fs::copy(Path::new(base).join(name), dest)
        .map(|_| ())
        .map_err(|e| format!("cannot read {name} from {base}: {e}"))
}

/// The expected SHA-256 for one named file in SHA256SUMS.
fn expected_sum(sums: &str, name: &str) -> Result<String, String> {
    for line in sums.lines() {
        let mut fields = line.split_whitespace();
        let (Some(hash), Some(file)) = (fields.next(), fields.next()) else {
            continue;
        };
        let file = file.strip_prefix('*').unwrap_or(file);
        if file == name && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Ok(hash.to_ascii_lowercase());
        }
    }
    Err(format!("no checksum for {name} in SHA256SUMS"))
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hex = String::with_capacity(64);
    for byte in Sha256::digest(bytes) {
        let _ = write!(&mut hex, "{byte:02x}");
    }
    hex
}

/// The command a person can run instead, for the install kind in use.
fn install_hint(kind: Option<&InstallKind>, version: &str) -> String {
    match kind {
        Some(InstallKind::Installer { .. }) => format!(
            "curl -fsSL https://github.com/{}/releases/latest/download/install.sh | RUSTY_MIRROR_VERSION={version} sh",
            github_slug()
        ),
        _ => format!(
            "cargo install --git {REPOSITORY} --tag v{version} --locked --force {PACKAGE}"
        ),
    }
}

/// A private build folder that is removed on success, failure and panic.
struct BuildDir(PathBuf);

impl BuildDir {
    fn create(temp: &Path) -> Result<BuildDir, String> {
        sweep_stale_builds(temp, now());
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        let dir = temp.join(format!("{BUILD_PREFIX}{}-{nanos}", std::process::id()));
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
        Ok(BuildDir(dir))
    }
}

impl Drop for BuildDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// A build killed mid-way leaves its folder; the next install removes any
/// such folder older than a day.
fn sweep_stale_builds(temp: &Path, now: u64) {
    let Ok(entries) = fs::read_dir(temp) else {
        return;
    };
    for entry in entries.flatten() {
        let ours = entry
            .file_name()
            .to_string_lossy()
            .starts_with(BUILD_PREFIX);
        let modified = entry.metadata().and_then(|m| m.modified()).ok();
        let age = modified
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |t| now.saturating_sub(t.as_secs()));
        if ours && age >= DAY {
            let _ = fs::remove_dir_all(entry.path());
        }
    }
}

/// Updates whichever kind of install owns the running binary.
struct Installer {
    kind: InstallKind,
    temp: PathBuf,
}

impl Installer {
    fn locate() -> Result<Installer, String> {
        let exe = env::current_exe().map_err(|e| format!("cannot find this binary: {e}"))?;
        let kind = InstallKind::locate(&exe).ok_or_else(|| {
            format!(
                "{} was not installed with cargo install or scripts/install.sh",
                exe.display()
            )
        })?;
        Ok(Installer {
            kind,
            temp: env::temp_dir(),
        })
    }

    fn install(&self, version: &str, current: &str, progress: bool) -> Result<(), String> {
        let tag = match parse_version(version) {
            Some(v) if Some(v) != parse_version(current) => format!("{}.{}.{}", v.0, v.1, v.2),
            _ => return Err(format!("refusing to install version {version:?}")),
        };
        match &self.kind {
            InstallKind::Cargo { cargo, root } => self.cargo_install(&tag, cargo, root, progress),
            InstallKind::Installer { home, bin } => {
                self.installer_install(&tag, current, home, bin, progress, None)
            }
        }
    }

    /// Cargo fetches the tag over HTTPS, builds exactly the tag's Cargo.lock
    /// (`--locked`, each crate checked against its recorded checksum) and only
    /// replaces the binary after a successful build.
    fn cargo_install(
        &self,
        tag: &str,
        cargo: &Path,
        root: &Path,
        progress: bool,
    ) -> Result<(), String> {
        let build = BuildDir::create(&self.temp)?;
        let mut command = Command::new(cargo);
        command
            .args(["install", "--git", REPOSITORY, "--tag"])
            .arg(format!("v{tag}"))
            .args(["--locked", "--force", "--root"])
            .arg(root)
            .arg("--target-dir")
            .arg(&build.0)
            .arg(PACKAGE)
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        let start = |e: io::Error| format!("cannot run {}: {e}", cargo.display());
        if progress {
            let status = command.status().map_err(start)?;
            return status
                .success()
                .then_some(())
                .ok_or(format!("cargo install v{tag} failed ({status})"));
        }
        let output = command.output().map_err(start)?;
        output
            .status
            .success()
            .then_some(())
            .ok_or_else(|| format!("cargo install v{tag} failed: {}", tail(&output.stderr)))
    }

    /// Download the installer, checksums and archive into a private folder,
    /// check both files before running anything, then let scripts/install.sh
    /// unpack and re-point `current`. Nothing outside `home` and `bin` changes.
    fn installer_install(
        &self,
        tag: &str,
        current: &str,
        home: &Path,
        bin: &Path,
        progress: bool,
        override_base: Option<&str>,
    ) -> Result<(), String> {
        let asset = platform_asset()?;
        let build = BuildDir::create(&self.temp)?;
        let base = release_base(tag, override_base);
        for name in ["install.sh", "SHA256SUMS", asset] {
            download_asset(&base, name, &build.0.join(name))?;
        }
        let sums = fs::read_to_string(build.0.join("SHA256SUMS"))
            .map_err(|e| format!("cannot read SHA256SUMS: {e}"))?;
        for name in ["install.sh", asset] {
            let want = expected_sum(&sums, name)?;
            let bytes =
                fs::read(build.0.join(name)).map_err(|e| format!("cannot read {name}: {e}"))?;
            if sha256_hex(&bytes) != want {
                return Err(format!(
                    "checksum mismatch for {name}; {current} is unchanged"
                ));
            }
        }
        let mut command = Command::new("sh");
        command
            .arg(build.0.join("install.sh"))
            .env("RUSTY_MIRROR_RELEASE_BASE", &build.0)
            .env("RUSTY_MIRROR_VERSION", tag)
            .env("RUSTY_MIRROR_HOME", home)
            .env("RUSTY_MIRROR_BIN_DIR", bin)
            .stdin(Stdio::null());
        if progress {
            let status = command
                .status()
                .map_err(|e| format!("cannot run install.sh: {e}"))?;
            return status.success().then_some(()).ok_or(format!(
                "install.sh failed ({status}); {current} is unchanged"
            ));
        }
        let output = command
            .output()
            .map_err(|e| format!("cannot run install.sh: {e}"))?;
        output.status.success().then_some(()).ok_or_else(|| {
            format!(
                "install.sh failed: {}; {current} is unchanged",
                tail(&output.stderr)
            )
        })
    }

    /// A script-install rollback only re-points `current`; no download or build.
    fn re_point(&self, version: &str) -> Result<(), String> {
        let home = match &self.kind {
            InstallKind::Installer { home, .. } => home,
            InstallKind::Cargo { .. } => return Ok(()),
        };
        let (major, minor, patch) =
            parse_version(version).ok_or_else(|| format!("invalid version {version:?}"))?;
        let folder = format!("{major}.{minor}.{patch}");
        let release = home.join("releases").join(&folder);
        if !release.join(TOOL).is_file() {
            return Err(format!(
                "previous release folder {} is missing",
                release.display()
            ));
        }
        let link = home.join("current");
        let fresh = home.join(format!(".current.update-{}", std::process::id()));
        let _ = fs::remove_file(&fresh);
        symlink(&release, &fresh).map_err(|e| format!("cannot link {}: {e}", fresh.display()))?;
        fs::rename(&fresh, &link)
            .map_err(|e| format!("cannot re-point {}: {e}", link.display()))?;
        Ok(())
    }
}

fn confirm(question: &str, default: bool) -> bool {
    eprint!("{question} ");
    let _ = io::stderr().flush();
    let mut answer = String::new();
    match io::stdin().read_line(&mut answer) {
        Ok(0) | Err(_) => default,
        Ok(_) => match answer.trim().to_ascii_lowercase().as_str() {
            "" => default,
            answer => answer.starts_with('y'),
        },
    }
}

/// What `rusty-mirror update` was asked to do.
pub(crate) enum Action<'a> {
    Install,
    Check,
    Skip,
    Auto(&'a str),
    Notify(&'a str),
    Rollback,
}

pub(crate) fn command(action: Action) -> ExitCode {
    let terminal = io::stdin().is_terminal() && io::stderr().is_terminal();
    let result = Store::locate().and_then(|store| match action {
        Action::Install => {
            install_latest(&store, &api_url(), Installer::locate(), CURRENT, terminal)
        }
        Action::Check => {
            let kind = Installer::locate().ok().map(|installer| installer.kind);
            check_report(&store, &api_url(), CURRENT, kind.as_ref())
        }
        Action::Skip => skip(&store, &api_url(), CURRENT),
        Action::Auto(value) => configure(&store, Some(value), None),
        Action::Notify(value) => configure(&store, None, Some(value)),
        Action::Rollback => rollback(&store, Installer::locate(), CURRENT, terminal),
    });
    match result {
        Ok((report, code)) => {
            crate::print(&report);
            ExitCode::from(code)
        }
        Err(error) => crate::fail(error),
    }
}

/// `update --check`: exit 0 up to date, 10 update available, 1 check failed.
fn check_report(
    store: &Store,
    url: &str,
    current: &str,
    kind: Option<&InstallKind>,
) -> Result<(Value, u8), String> {
    let state = check(store, url)?;
    let new = available(&state, current);
    let command = new.map(|version| install_hint(kind, version));
    let report = json!({
        "ok": true, "current": current, "latest": state.latest,
        "updateAvailable": new.is_some(), "notes": new.and(state.notes.as_ref()),
        "skipped": state.skipped, "command": command,
    });
    Ok((report, if new.is_some() { 10 } else { 0 }))
}

fn skip(store: &Store, url: &str, current: &str) -> Result<(Value, u8), String> {
    let mut state = store.load();
    if available(&state, current).is_none() {
        state = check(store, url)?;
    }
    let Some(new) = available(&state, current).map(str::to_owned) else {
        return Ok((
            json!({"ok": true, "current": current, "updateAvailable": false}),
            0,
        ));
    };
    state.skipped = Some(new.clone());
    store.save(&state)?;
    Ok((json!({"ok": true, "skipped": new}), 0))
}

/// `update --auto on|off|status` and `update --notify on|off|status`.
fn configure(
    store: &Store,
    auto: Option<&str>,
    notify: Option<&str>,
) -> Result<(Value, u8), String> {
    let mut state = store.load();
    match (auto, notify) {
        (Some("on"), _) => {
            state.auto = true;
            store.save(&state)?;
        }
        (Some("off"), _) => {
            store.forget(&state)?;
            state = store.load();
        }
        (_, Some(value @ ("on" | "off"))) => {
            state.notify = value == "on";
            store.save(&state)?;
        }
        _ => {}
    }
    let file = store.state_path();
    let report = json!({
        "ok": true, "current": CURRENT, "notify": state.notify, "auto": state.auto,
        "lastCheckError": state.error, "lastAutoInstallError": state.auto_error,
        "stateFile": file.exists().then_some(file),
    });
    Ok((report, 0))
}

/// `update`: check now, confirm on a terminal, install, then offer auto once.
fn install_latest(
    store: &Store,
    url: &str,
    installer: Result<Installer, String>,
    current: &str,
    terminal: bool,
) -> Result<(Value, u8), String> {
    let state = check(store, url)?;
    let Some(new) = available(&state, current).map(str::to_owned) else {
        return Ok((
            json!({"ok": true, "current": current, "updateAvailable": false}),
            0,
        ));
    };
    let kind = installer.as_ref().ok().map(|i| i.kind.clone());
    let installer = installer
        .map_err(|e| format!("{e}; to update, run: {}", install_hint(kind.as_ref(), &new)))?;
    if terminal {
        eprintln!("{TOOL} {current} → {new}");
        for line in state.notes.iter().flat_map(|notes| notes.lines()) {
            eprintln!("  {line}");
        }
        if !confirm(&format!("Install {new} now? [Y/n]"), true) {
            return Ok((json!({"ok": true, "installed": false, "latest": new}), 0));
        }
    }
    installer
        .install(&new, current, terminal)
        .map_err(|e| format!("{e}; {current} is unchanged"))?;
    let mut state = store.load();
    state.previous = Some(current.into());
    state.auto_error = None;
    if terminal && !state.auto && !state.auto_asked {
        state.auto = confirm("Install future updates automatically? [y/N]", false);
        state.auto_asked = true;
    }
    store.save(&state)?;
    let report = json!({
        "ok": true, "installed": new, "previous": current, "auto": state.auto,
        "note": "the new version runs from your next command",
    });
    Ok((report, 0))
}

/// `update --rollback`: restore the version that ran before the last update.
/// Script installs only re-point `current`; Cargo installs rebuild the tag.
fn rollback(
    store: &Store,
    installer: Result<Installer, String>,
    current: &str,
    terminal: bool,
) -> Result<(Value, u8), String> {
    let kind = installer.as_ref().ok().map(|i| i.kind.clone());
    let previous = store.load().previous.ok_or_else(|| {
        format!(
            "no earlier version is recorded; to install one, run: {}",
            install_hint(kind.as_ref(), "X.Y.Z")
        )
    })?;
    let installer = installer?;
    match &installer.kind {
        InstallKind::Installer { .. } => installer
            .re_point(&previous)
            .map_err(|e| format!("{e}; {current} is unchanged"))?,
        InstallKind::Cargo { .. } => installer
            .install(&previous, current, terminal)
            .map_err(|e| format!("{e}; {current} is unchanged"))?,
    }
    let mut state = store.load();
    state.previous = None;
    state.skipped = Some(current.into()); // do not offer the version just left
    store.save(&state)?;
    let report = json!({
        "ok": true, "installed": previous, "skipped": current,
        "note": "the restored version runs from your next command",
    });
    Ok((report, 0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::Read,
        net::TcpListener,
        os::unix::fs::PermissionsExt,
        thread::{self, JoinHandle},
        time::Duration,
    };

    /// A private test folder, removed even when an assertion fails.
    struct TestDir(PathBuf);

    impl TestDir {
        fn new(name: &str) -> TestDir {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos();
            let dir =
                env::temp_dir().join(format!("rm-update-{name}-{}-{nanos}", std::process::id()));
            fs::create_dir(&dir).unwrap();
            TestDir(dir)
        }

        fn store(&self) -> Store {
            Store {
                dir: self.0.join("cache"),
            }
        }

        /// A cargo install root holding an "old" binary, and a fake cargo that
        /// records its arguments, writes into `--target-dir` and runs `body`.
        fn installer(&self, body: &str) -> Installer {
            let root = self.0.join("root");
            fs::create_dir_all(root.join("bin")).unwrap();
            fs::write(root.join("bin").join(TOOL), "old").unwrap();
            let temp = self.0.join("tmp");
            fs::create_dir_all(&temp).unwrap();
            let cargo = self.0.join("fake-cargo");
            let script = format!(
                "#!/bin/sh\nhere=$(dirname \"$0\")\nprintf '%s\\n' \"$@\" > \"$here/args\"\n\
                 prev=\nfor arg in \"$@\"; do\n  if [ \"$prev\" = --target-dir ]; then echo partial > \"$arg/obj\"; fi\n\
                 if [ \"$prev\" = --root ]; then root=$arg; fi\n  prev=$arg\ndone\n{body}\n"
            );
            fs::write(&cargo, script).unwrap();
            fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
            Installer {
                kind: InstallKind::Cargo { cargo, root },
                temp,
            }
        }

        fn args(&self) -> Vec<String> {
            fs::read_to_string(self.0.join("args"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }

        fn binary(&self) -> String {
            fs::read_to_string(self.0.join("root").join("bin").join(TOOL)).unwrap()
        }

        fn temp_is_empty(&self) -> bool {
            fs::read_dir(self.0.join("tmp")).unwrap().next().is_none()
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const CARGO_OK: &str = "echo new > \"$root/bin/rusty-mirror\"; exit 0";
    const CARGO_BAD_CHECKSUM: &str =
        "echo 'error: checksum for `serde v1.0.0` changed between lock files' >&2; exit 101";

    fn release(tag: &str) -> String {
        json!({"tag_name": tag, "draft": false, "prerelease": false, "body": "## Fixes\n- Faster capture\n"})
            .to_string()
    }

    /// A one-shot local release API that hands back the raw request it saw.
    fn serve(body: String) -> (String, JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!(
            "http://{}/repos/o/r/releases/latest",
            listener.local_addr().unwrap()
        );
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = Vec::new();
            let mut buf = [0; 4096];
            while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                let n = stream.read(&mut buf).unwrap();
                if n == 0 {
                    break;
                }
                request.extend_from_slice(&buf[..n]);
            }
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(body.as_bytes()).unwrap();
            String::from_utf8_lossy(&request).into_owned()
        });
        (url, server)
    }

    fn closed_port_url() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/releases/latest", listener.local_addr().unwrap())
    }

    #[test]
    fn versions_compare_strictly_on_stable_releases() {
        assert!(is_newer("0.4.0", "0.3.0"));
        assert!(is_newer("v1.0.0", "0.99.99"));
        assert!(is_newer("0.3.10", "0.3.9"));
        assert!(!is_newer("0.3.0", "0.3.0"));
        assert!(!is_newer("v0.3.0", "0.3.0"));
        assert!(!is_newer("0.2.9", "0.3.0"));
        for bad in [
            "",
            "v",
            "garbage",
            "1.0",
            "1.0.0.0",
            "1.0.0-rc.1",
            "1.0.0+b",
            "01.0.0",
            "1..0",
            "-1.0.0",
            " 1.0.0",
        ] {
            assert_eq!(parse_version(bad), None, "{bad:?} parsed");
            assert!(!is_newer(bad, "0.1.0"), "{bad:?} counted as newer");
        }
        assert!(!is_newer("9.9.9", "garbage"));
    }

    #[test]
    fn release_record_ignores_drafts_prereleases_and_odd_tags() {
        for raw in [
            r#"{"tag_name":"v9.9.9","draft":true}"#,
            r#"{"tag_name":"v9.9.9","prerelease":true}"#,
            r#"{"tag_name":"v9.9.9-beta.1"}"#,
            r#"{"tag_name":"nightly","body":null}"#,
        ] {
            assert_eq!(parse_release(raw).unwrap(), None, "{raw}");
        }
        assert!(parse_release("<html>rate limited</html>").is_err());
        let (version, notes) = parse_release(&release("v9.9.9")).unwrap().unwrap();
        assert_eq!(version, "9.9.9");
        assert_eq!(notes, "Fixes\nFaster capture");
        let long = format!("{}\n\n- a\n- b\n- c\n- d\n- e\n- f", "x".repeat(300));
        let notes = short_notes(&long);
        assert_eq!(notes.lines().count(), 5);
        assert_eq!(notes.lines().next().unwrap().len(), 100);
    }

    #[test]
    fn automatic_checks_only_for_a_person_at_a_terminal() {
        let state = State::default();
        fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
            let pairs: Vec<(String, String)> = pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect();
            move |key| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
        }
        assert!(automatic(true, &state, env(&[])));
        assert!(!automatic(false, &state, env(&[])), "no terminal");
        assert!(!automatic(true, &state, env(&[("CI", "true")])));
        assert!(!automatic(true, &state, env(&[("CI", "1")])));
        assert!(automatic(true, &state, env(&[("CI", "0")])));
        assert!(automatic(true, &state, env(&[("CI", "false")])));
        assert!(!automatic(
            true,
            &state,
            env(&[("RUSTY_MIRROR_NO_UPDATE_CHECK", "1")])
        ));
        assert!(automatic(
            true,
            &state,
            env(&[("RUSTY_MIRROR_NO_UPDATE_CHECK", "0")])
        ));
        for agent in ["CLAUDECODE", "CODEX_SANDBOX", "GEMINI_CLI"] {
            assert!(!automatic(true, &state, env(&[(agent, "1")])), "{agent}");
        }
        let opted_out = State {
            notify: false,
            ..State::default()
        };
        assert!(!automatic(true, &opted_out, env(&[])), "--notify off");
    }

    #[test]
    fn check_is_one_anonymous_get_and_is_cached_privately() {
        let dir = TestDir::new("check");
        let store = dir.store();
        let (url, server) = serve(release("v9.9.9"));
        let state = check(&store, &url).unwrap();
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(
            request.starts_with("get /repos/o/r/releases/latest http/1.1\r\n"),
            "{request}"
        );
        assert!(request.contains(&format!("\r\nuser-agent: {USER_AGENT}\r\n")));
        assert!(!request.contains(CURRENT), "the request carries no version");
        for header in ["authorization", "cookie", "x-"] {
            assert!(!request.contains(&format!("\r\n{header}")), "{header} sent");
        }
        assert_eq!(state.latest.as_deref(), Some("9.9.9"));
        assert_eq!(available(&state, "0.3.0"), Some("9.9.9"));
        assert!(!due(state.checked_at, now()));
        assert_eq!(store.load(), state);
        let mode = fs::metadata(store.state_path())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        let dir_mode = fs::metadata(&store.dir).unwrap().permissions().mode();
        assert_eq!(dir_mode & 0o777, 0o700);
        let files: Vec<_> = fs::read_dir(&store.dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(
            files,
            ["update.json"],
            "the check writes one file and nothing else"
        );
    }

    #[test]
    fn failed_check_records_its_cause_and_waits_a_day() {
        let dir = TestDir::new("fail");
        let store = dir.store();
        store
            .save(&State {
                latest: Some("9.9.9".into()),
                ..State::default()
            })
            .unwrap();
        let cause = check(&store, &closed_port_url()).unwrap_err();
        assert!(cause.starts_with("release check failed: "), "{cause}");
        let state = store.load();
        assert_eq!(state.error.as_deref(), Some(cause.as_str()));
        assert!(!due(state.checked_at, now()), "no retry loop");
        assert_eq!(state.latest.as_deref(), Some("9.9.9"), "known release kept");
        let (report, code) = configure(&store, Some("status"), None).unwrap();
        assert_eq!(report["lastCheckError"], json!(cause));
        assert_eq!(code, 0);
    }

    #[test]
    fn checks_and_notices_follow_a_daily_interval() {
        let now = 1_900_000_000;
        assert!(due(None, now));
        assert!(!due(Some(now - DAY + 1), now));
        assert!(due(Some(now - DAY), now));
        assert!(
            due(Some(now + 60), now),
            "a clock set back does not stall checks"
        );
    }

    #[test]
    fn notice_text_skip_and_once_a_day_per_version() {
        let now = 1_900_000_000;
        let state = State {
            latest: Some("0.4.0".into()),
            ..State::default()
        };
        let [first, second] = notice(&state, "0.3.0", now).unwrap();
        assert_eq!(
            first,
            "rusty-mirror 0.4.0 available (you have 0.3.0) · run: rusty-mirror update"
        );
        assert_eq!(
            second,
            "turn on automatic updates: rusty-mirror update --auto on · skip: rusty-mirror update --skip"
        );
        let auto = State {
            auto: true,
            ..state.clone()
        };
        assert_eq!(
            notice(&auto, "0.3.0", now).unwrap()[1],
            "skip: rusty-mirror update --skip"
        );
        assert!(notice(&state, "0.4.0", now).is_none(), "not newer");
        assert!(notice(&state, "0.5.0", now).is_none(), "older");
        let shown = State {
            shown: Some("0.4.0".into()),
            shown_at: Some(now),
            ..state.clone()
        };
        assert!(notice(&shown, "0.3.0", now + DAY - 1).is_none());
        assert!(notice(&shown, "0.3.0", now + DAY).is_some());
        let newer_than_shown = State {
            latest: Some("0.5.0".into()),
            ..shown.clone()
        };
        assert!(notice(&newer_than_shown, "0.3.0", now + 1).is_some());
        let skipped = State {
            skipped: Some("0.4.0".into()),
            ..state.clone()
        };
        assert!(notice(&skipped, "0.3.0", now).is_none());
        let newer_than_skipped = State {
            latest: Some("0.4.1".into()),
            ..skipped
        };
        assert!(notice(&newer_than_skipped, "0.3.0", now).is_some());
    }

    #[test]
    fn notice_has_a_dim_second_line_only_with_colour() {
        let lines = ["one".to_string(), "two".to_string()];
        let mut out = Vec::new();
        write_notice(&mut out, &lines, true).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "one\n\x1b[2mtwo\x1b[0m\n");
        let mut out = Vec::new();
        write_notice(&mut out, &lines, false).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "one\ntwo\n");
    }

    #[test]
    fn skip_hides_only_the_current_release() {
        let dir = TestDir::new("skip");
        let store = dir.store();
        let (url, server) = serve(release("v0.4.0"));
        let (report, code) = skip(&store, &url, "0.3.0").unwrap();
        server.join().unwrap();
        assert_eq!((report["skipped"].clone(), code), (json!("0.4.0"), 0));
        assert!(notice(&store.load(), "0.3.0", now()).is_none());
        let (url, server) = serve(release("v0.4.0"));
        let (report, _) = skip(&store, &url, "0.4.0").unwrap();
        server.join().unwrap();
        assert_eq!(
            report["updateAvailable"],
            json!(false),
            "up to date: nothing to skip"
        );
    }

    #[test]
    fn auto_is_off_by_default_and_persists() {
        let dir = TestDir::new("auto");
        let store = dir.store();
        assert!(!store.load().auto && store.load().notify);
        let (report, _) = configure(&store, Some("status"), None).unwrap();
        assert_eq!(
            (report["auto"].clone(), report["notify"].clone()),
            (json!(false), json!(true))
        );
        assert!(!store.state_path().exists(), "status writes nothing");
        configure(&store, Some("on"), None).unwrap();
        assert!(store.load().auto);
        let (report, _) = configure(&store, None, Some("status")).unwrap();
        assert_eq!(report["auto"], json!(true));
    }

    #[test]
    fn auto_off_deletes_the_cache_and_log_but_keeps_notify_off() {
        let dir = TestDir::new("off");
        let store = dir.store();
        store
            .save(&State {
                auto: true,
                latest: Some("9.9.9".into()),
                skipped: Some("9.9.9".into()),
                ..State::default()
            })
            .unwrap();
        store.log("installing 9.9.9");
        let (report, _) = configure(&store, Some("off"), None).unwrap();
        assert_eq!(report["stateFile"], json!(null));
        assert!(!store.dir.exists(), "--auto off leaves nothing behind");
        configure(&store, None, Some("off")).unwrap();
        configure(&store, Some("off"), None).unwrap();
        assert_eq!(
            store.load(),
            State {
                notify: false,
                ..State::default()
            }
        );
        configure(&store, None, Some("on")).unwrap();
        configure(&store, Some("off"), None).unwrap();
        assert!(!store.dir.exists());
    }

    #[test]
    fn log_stays_under_256_kib_at_line_boundaries() {
        let dir = TestDir::new("log");
        let store = dir.store();
        for i in 0..400 {
            store.log(&format!("line {i} {}", "x".repeat(2000)));
            assert!(fs::metadata(store.log_path()).unwrap().len() as usize <= LOG_LIMIT);
        }
        let log = fs::read_to_string(store.log_path()).unwrap();
        assert!(log.lines().next().unwrap().split(' ').nth(1) == Some("line"));
        assert!(log.lines().last().unwrap().contains("line 399 "));
        assert_eq!(fs::read_dir(&store.dir).unwrap().count(), 1, "one log file");
    }

    #[test]
    fn update_runs_cargo_for_the_tag_and_removes_its_build_folder() {
        let dir = TestDir::new("install");
        let store = dir.store();
        let installer = dir.installer(CARGO_OK);
        let root = match &installer.kind {
            InstallKind::Cargo { root, .. } => root.clone(),
            InstallKind::Installer { .. } => unreachable!(),
        };
        let temp = installer.temp.clone();
        let (url, server) = serve(release("v0.4.0"));
        let (report, code) = install_latest(&store, &url, Ok(installer), "0.3.0", false).unwrap();
        server.join().unwrap();
        assert_eq!((report["installed"].clone(), code), (json!("0.4.0"), 0));
        let args = dir.args();
        let target = args[args.iter().position(|a| a == "--target-dir").unwrap() + 1].clone();
        assert_eq!(
            args,
            [
                "install",
                "--git",
                REPOSITORY,
                "--tag",
                "v0.4.0",
                "--locked",
                "--force",
                "--root",
                &root.to_string_lossy(),
                "--target-dir",
                &target,
                PACKAGE,
            ]
        );
        assert!(Path::new(&target).starts_with(&temp));
        assert!(
            !Path::new(&target).exists() && dir.temp_is_empty(),
            "build folder removed"
        );
        assert_eq!(dir.binary().trim(), "new");
        let state = store.load();
        assert_eq!(state.previous.as_deref(), Some("0.3.0"));
        assert!(
            !state.auto && !state.auto_asked,
            "no question without a terminal"
        );
    }

    #[test]
    fn failed_install_keeps_the_current_version_and_leaves_no_build_folder() {
        let dir = TestDir::new("refuse");
        let store = dir.store();
        let (url, server) = serve(release("v0.4.0"));
        let installer = dir.installer(CARGO_BAD_CHECKSUM);
        let error = install_latest(&store, &url, Ok(installer), "0.3.0", false).unwrap_err();
        server.join().unwrap();
        assert!(
            error.contains("checksum for `serde v1.0.0` changed"),
            "{error}"
        );
        assert!(error.ends_with("0.3.0 is unchanged"), "{error}");
        assert_eq!(dir.binary(), "old");
        assert!(dir.temp_is_empty(), "build folder removed after a failure");
        assert_eq!(store.load().previous, None);
    }

    #[test]
    fn odd_or_current_versions_are_refused_without_running_cargo() {
        let dir = TestDir::new("odd");
        let installer = dir.installer(CARGO_OK);
        for version in ["0.3.0", "v0.3.0", "0.4.0;rm -rf x", "0.4.0-rc.1", ""] {
            assert!(
                installer.install(version, "0.3.0", false).is_err(),
                "{version:?}"
            );
        }
        assert!(dir.args().is_empty() && dir.temp_is_empty());
    }

    #[test]
    fn rollback_reinstalls_the_previous_tag_and_skips_the_version_left() {
        let dir = TestDir::new("rollback");
        let store = dir.store();
        let installer = dir.installer(CARGO_OK);
        let error = rollback(&store, Ok(dir.installer(CARGO_OK)), "0.4.0", false).unwrap_err();
        assert!(error.contains("cargo install --git"), "{error}");
        store
            .save(&State {
                previous: Some("0.3.0".into()),
                latest: Some("0.4.0".into()),
                ..State::default()
            })
            .unwrap();
        let (report, _) = rollback(&store, Ok(installer), "0.4.0", false).unwrap();
        assert_eq!(report["installed"], json!("0.3.0"));
        assert!(dir.args().contains(&"v0.3.0".to_string()));
        assert_eq!(dir.binary().trim(), "new");
        let state = store.load();
        assert_eq!(
            (state.previous, state.skipped.as_deref()),
            (None, Some("0.4.0"))
        );
        assert!(
            notice(&store.load(), "0.3.0", now()).is_none(),
            "the version left is not offered"
        );
        assert!(dir.temp_is_empty());
    }

    #[test]
    fn stale_build_folders_are_swept() {
        let dir = TestDir::new("sweep");
        let old = dir.0.join(format!("{BUILD_PREFIX}1-1"));
        let fresh = dir.0.join(format!("{BUILD_PREFIX}2-2"));
        let other = dir.0.join("someone-else");
        for path in [&old, &fresh, &other] {
            fs::create_dir(path).unwrap();
        }
        let two_days_ago = SystemTime::now() - Duration::from_secs(2 * DAY);
        for path in [&old, &other] {
            fs::File::open(path)
                .unwrap()
                .set_modified(two_days_ago)
                .unwrap();
        }
        sweep_stale_builds(&dir.0, now());
        assert!(!old.exists() && fresh.exists() && other.exists());
    }

    #[test]
    fn only_cargo_installs_are_updated_in_place() {
        let dir = TestDir::new("root");
        let bin = dir.0.join("bin");
        fs::create_dir(&bin).unwrap();
        let exe = bin.join(TOOL);
        fs::write(&exe, "").unwrap();
        assert_eq!(cargo_root(&exe), None, "no Cargo records");
        let key = format!("{PACKAGE} 0.3.0 (git+{REPOSITORY}?tag=v0.3.0#abc)");
        fs::write(
            dir.0.join(".crates2.json"),
            json!({"installs": {key: {}}}).to_string(),
        )
        .unwrap();
        assert_eq!(cargo_root(&exe), Some(fs::canonicalize(&dir.0).unwrap()));
        let elsewhere = dir.0.join(TOOL);
        fs::write(&elsewhere, "").unwrap();
        assert_eq!(cargo_root(&elsewhere), None, "not in a bin folder");
        let store = dir.store();
        let (url, server) = serve(release("v0.4.0"));
        let error = install_latest(
            &store,
            &url,
            Err("not a cargo install".into()),
            "0.3.0",
            false,
        )
        .unwrap_err();
        server.join().unwrap();
        assert!(error.ends_with(&install_hint(None, "0.4.0")), "{error}");
        assert!(install_hint(None, "0.4.0").contains("--tag v0.4.0 --locked"));
    }

    #[test]
    fn auto_install_tries_once_logs_and_falls_back_to_the_notice() {
        let dir = TestDir::new("background");
        let store = dir.store();
        let pending = State {
            auto: true,
            latest: Some("0.4.0".into()),
            ..State::default()
        };
        store.save(&pending).unwrap();
        auto_install(&store, Ok(dir.installer(CARGO_BAD_CHECKSUM)), "0.3.0");
        let state = store.load();
        assert!(state
            .auto_error
            .as_deref()
            .is_some_and(|e| e.contains("checksum")));
        assert!(!auto_pending(&state, "0.3.0"), "not retried in a loop");
        assert!(
            notice(&state, "0.3.0", now()).is_some(),
            "falls back to the notice"
        );
        assert_eq!(dir.binary(), "old");
        let log = fs::read_to_string(store.log_path()).unwrap();
        assert!(log.contains("installing 0.4.0") && log.contains("install of 0.4.0 failed"));
        fs::remove_file(dir.0.join("args")).unwrap();
        auto_install(&store, Ok(dir.installer(CARGO_OK)), "0.3.0");
        assert!(dir.args().is_empty(), "same version not attempted twice");

        store
            .save(&State {
                latest: Some("0.4.1".into()),
                ..state
            })
            .unwrap();
        auto_install(&store, Ok(dir.installer(CARGO_OK)), "0.3.0");
        let state = store.load();
        assert_eq!(dir.binary().trim(), "new");
        assert_eq!(state.previous.as_deref(), Some("0.3.0"));
        assert_eq!(state.auto_error, None);
        assert!(fs::read_to_string(store.log_path())
            .unwrap()
            .contains("installed 0.4.1"));
        assert!(dir.temp_is_empty());

        let off = State {
            auto: false,
            latest: Some("0.5.0".into()),
            ..State::default()
        };
        store.save(&off).unwrap();
        fs::remove_file(dir.0.join("args")).unwrap();
        auto_install(&store, Ok(dir.installer(CARGO_OK)), "0.3.0");
        assert!(dir.args().is_empty(), "nothing installs while auto is off");
    }

    /// A real scripts/install.sh layout: marker, versioned folder, current link and shim.
    fn installer_layout(dir: &TestDir, version: &str, bin: &Path) -> (PathBuf, PathBuf) {
        let home = dir.0.join("home");
        fs::create_dir_all(&home).unwrap();
        let home = fs::canonicalize(&home).unwrap();
        let release = home.join("releases").join(version);
        fs::create_dir_all(&release).unwrap();
        fs::write(release.join(TOOL), format!("binary {version}")).unwrap();
        fs::write(
            home.join(INSTALLER_MARKER),
            "Created by the rusty-mirror installer.\n",
        )
        .unwrap();
        let current = home.join("current");
        let _ = fs::remove_file(&current);
        symlink(&release, &current).unwrap();
        fs::create_dir_all(bin).unwrap();
        fs::write(
            bin.join(TOOL),
            format!(
                "#!/bin/sh\n{SHIM_MARKER}\nexec \"{}/current/rusty-mirror\" \"$@\"\n",
                home.display()
            ),
        )
        .unwrap();
        (fs::canonicalize(release.join(TOOL)).unwrap(), home)
    }

    /// A fake release directory whose checksums match unless `corrupt` is set.
    fn installer_release(dir: &TestDir, script: &str, corrupt: bool) -> (PathBuf, String) {
        let base = dir.0.join("release");
        fs::create_dir_all(&base).unwrap();
        let asset = platform_asset().unwrap();
        fs::write(base.join("install.sh"), script).unwrap();
        fs::write(base.join(asset), "archive bytes").unwrap();
        let mut sum = sha256_hex(script.as_bytes());
        if corrupt {
            sum = "0".repeat(64);
        }
        fs::write(
            base.join("SHA256SUMS"),
            format!(
                "{sum}  install.sh\n{}  {asset}\n",
                sha256_hex(b"archive bytes")
            ),
        )
        .unwrap();
        let base_str = base.to_string_lossy().into_owned();
        (base, base_str)
    }

    fn installer_at(dir: &TestDir, kind: InstallKind) -> Installer {
        let temp = dir.0.join("tmp");
        fs::create_dir_all(&temp).unwrap();
        Installer { kind, temp }
    }

    #[test]
    fn installer_layout_is_detected_and_foreign_homes_are_refused() {
        let dir = TestDir::new("layout");
        let bin = dir.0.join("bin");
        let (exe, home) = installer_layout(&dir, "0.3.0", &bin);
        let detected = installer_home_from(&exe, Some(&bin));
        assert_eq!(
            detected,
            Some(InstallKind::Installer {
                home: home.clone(),
                bin: bin.clone()
            })
        );
        let hint = install_hint(detected.as_ref(), "0.4.0");
        assert!(
            hint.contains("/releases/latest/download/install.sh"),
            "{hint}"
        );
        assert!(hint.contains("RUSTY_MIRROR_VERSION=0.4.0"), "{hint}");
        fs::remove_file(home.join(INSTALLER_MARKER)).unwrap();
        assert_eq!(installer_home_from(&exe, Some(&bin)), None, "no marker");
        assert_eq!(
            installer_home_from(&dir.0.join(TOOL), Some(&bin)),
            None,
            "not an installer binary"
        );
    }

    #[test]
    fn installer_update_verifies_then_runs_installer_with_its_environment() {
        let dir = TestDir::new("installer-update");
        let bin = dir.0.join("bin");
        let (exe, home) = installer_layout(&dir, "0.3.0", &bin);
        let installer = installer_at(&dir, installer_home_from(&exe, Some(&bin)).unwrap());
        let script = "#!/bin/sh\nprintf '%s\\n' \"$RUSTY_MIRROR_VERSION\" \"$RUSTY_MIRROR_HOME\" \"$RUSTY_MIRROR_BIN_DIR\" \"$RUSTY_MIRROR_RELEASE_BASE\" > \"$RUSTY_MIRROR_HOME/install-args\"\n";
        let (_, base) = installer_release(&dir, script, false);
        installer
            .installer_install("0.4.0", "0.3.0", &home, &bin, false, Some(&base))
            .unwrap();
        let args: Vec<_> = fs::read_to_string(home.join("install-args"))
            .unwrap()
            .lines()
            .map(str::to_owned)
            .collect();
        assert_eq!(args[0], "0.4.0");
        assert_eq!(args[1], home.to_string_lossy());
        assert_eq!(args[2], bin.to_string_lossy());
        assert!(
            Path::new(&args[3]).starts_with(&installer.temp)
                && Path::new(&args[3])
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(BUILD_PREFIX),
            "the private build folder is passed as the release base: {}",
            args[3]
        );
        assert_eq!(
            fs::read_dir(&installer.temp).unwrap().count(),
            0,
            "build folder removed"
        );
    }

    #[test]
    fn checksum_mismatch_refuses_to_run_the_installer() {
        let dir = TestDir::new("installer-checksum");
        let bin = dir.0.join("bin");
        let (exe, home) = installer_layout(&dir, "0.3.0", &bin);
        let installer = installer_at(&dir, installer_home_from(&exe, Some(&bin)).unwrap());
        let script = "#!/bin/sh\ntouch \"$RUSTY_MIRROR_HOME/ran\"\n";
        let (_, base) = installer_release(&dir, script, true);
        let error = installer
            .installer_install("0.4.0", "0.3.0", &home, &bin, false, Some(&base))
            .unwrap_err();
        assert!(
            error.contains("checksum mismatch for install.sh"),
            "{error}"
        );
        assert!(error.contains("0.3.0 is unchanged"), "{error}");
        assert!(!home.join("ran").exists(), "script never ran");
        assert_eq!(fs::read_dir(&installer.temp).unwrap().count(), 0);
    }

    #[test]
    fn installer_rollback_repoints_current_to_the_previous_folder() {
        let dir = TestDir::new("installer-rollback");
        let bin = dir.0.join("bin");
        let (exe, home) = installer_layout(&dir, "0.4.0", &bin);
        let previous = home.join("releases").join("0.3.0");
        fs::create_dir_all(&previous).unwrap();
        fs::write(previous.join(TOOL), "binary 0.3.0").unwrap();
        let installer = installer_at(&dir, installer_home_from(&exe, Some(&bin)).unwrap());
        let store = dir.store();
        store
            .save(&State {
                previous: Some("0.3.0".into()),
                latest: Some("0.4.0".into()),
                ..State::default()
            })
            .unwrap();
        let (report, code) = rollback(&store, Ok(installer), "0.4.0", false).unwrap();
        assert_eq!((report["installed"].clone(), code), (json!("0.3.0"), 0));
        assert_eq!(
            fs::canonicalize(home.join("current")).unwrap(),
            fs::canonicalize(previous).unwrap()
        );
        let state = store.load();
        assert_eq!(state.previous, None);
        assert_eq!(state.skipped.as_deref(), Some("0.4.0"));
    }
}
