//! Update notices and opt-in self-update (update contract v1, docs/updates.md).
//!
//! `update` installs the same way the CLI was installed. A copy from
//! scripts/install.sh is replaced by the release archive for this platform,
//! checked against the release's SHA256SUMS before anything is unpacked, then
//! swapped in with one atomic link rename; the release it replaced is kept for
//! `update --rollback` until the new one has run once. A `cargo install --git …
//! --tag vX --locked` copy is owned by Cargo, so `update` runs that command for
//! the new tag. The only automatic network request is one anonymous GET of the
//! public latest-release record, at most once a day, from a detached helper.
//! State is one small 0600 file in the platform cache directory.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
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
/// Written by scripts/install.sh into the folder it owns.
const INSTALLER_MARKER: &str = ".installed-by-rusty-mirror";
/// The replaced release goes once the new one has run, or after this long.
const ROLLBACK_KEEP: u64 = 7 * DAY;

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

/// The scripts/install.sh folder that owns `exe`: `<dir>/releases/<x.y.z>/rusty-mirror`,
/// with the installer's marker, and only while `<dir>/current` points at that release.
fn script_dir(exe: &Path) -> Option<PathBuf> {
    let exe = fs::canonicalize(exe).ok()?;
    let release = exe.parent()?;
    parse_version(release.file_name()?.to_str()?)?;
    let releases = release
        .parent()
        .filter(|r| r.file_name() == Some("releases".as_ref()))?;
    let dir = releases.parent()?;
    let current = fs::canonicalize(dir.join("current")).ok()?;
    (dir.join(INSTALLER_MARKER).is_file() && current == release).then(|| dir.to_path_buf())
}

fn github_slug() -> &'static str {
    REPOSITORY.trim_start_matches("https://github.com/")
}

/// The archive scripts/build-release.sh publishes for this machine.
fn platform_asset() -> Result<&'static str, String> {
    match (env::consts::OS, env::consts::ARCH) {
        ("macos", _) => Ok("rusty-mirror-darwin-universal.tar.gz"),
        ("linux", "x86_64") => Ok("rusty-mirror-linux-x86_64.tar.gz"),
        (os, arch) => Err(format!("no release download for {os} {arch}")),
    }
}

/// The command a person can run instead, for the install kind in use.
fn install_hint(kind: Option<&Kind>, version: &str) -> String {
    match kind {
        Some(Kind::Script { .. }) => format!(
            "curl -fsSL https://github.com/{}/releases/latest/download/install.sh | RUSTY_MIRROR_VERSION={version} sh",
            github_slug()
        ),
        _ => format!("cargo install --git {REPOSITORY} --tag v{version} --locked --force {PACKAGE}"),
    }
}

/// A release file from a URL (curl, fixed User-Agent, no `~/.curlrc`) or, for
/// tests, from a local directory.
fn download(base: &str, name: &str, dest: &Path) -> Result<(), String> {
    if !["https://", "http://", "file://"]
        .iter()
        .any(|scheme| base.starts_with(scheme))
    {
        return fs::copy(Path::new(base).join(name), dest)
            .map(|_| ())
            .map_err(|e| format!("cannot copy {name} from {base}: {e}"));
    }
    let mut curl = Command::new("curl");
    curl.args([
        "--disable",
        "--silent",
        "--show-error",
        "--fail",
        "--location",
    ])
    .args(["--max-redirs", "5", "--max-time", "300", "--retry", "2"])
    .args(["--user-agent", USER_AGENT]);
    if base.starts_with("https://") {
        curl.args(["--proto", "=https", "--proto-redir", "=https"]);
    }
    let output = curl
        .arg("--output")
        .arg(dest)
        .arg(format!("{base}/{name}"))
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run curl: {e}"))?;
    output
        .status
        .success()
        .then_some(())
        .ok_or_else(|| format!("download of {name} failed: {}", tail(&output.stderr)))
}

/// The SHA-256 that a `SHA256SUMS` file lists for `name`.
fn listed_sum(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        let (hash, file) = (fields.next()?, fields.next()?);
        let hex = hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit());
        (hex && file.trim_start_matches('*') == name).then(|| hash.to_ascii_lowercase())
    })
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

/// Point `<dir>/current` at `release` with one rename, so a command starting
/// at the same moment runs either the old release or the new one, never neither.
fn swap_current(dir: &Path, release: &Path) -> Result<(), String> {
    let fresh = dir.join(format!(".current.{}", std::process::id()));
    let _ = fs::remove_file(&fresh);
    symlink(release, &fresh)
        .and_then(|()| fs::rename(&fresh, dir.join("current")))
        .map_err(|e| {
            let _ = fs::remove_file(&fresh);
            format!("cannot switch {}: {e}", dir.join("current").display())
        })
}

/// Remove every release folder except `keep` (and the current one).
fn prune_releases(dir: &Path, keep: &[&Path]) {
    let current = fs::canonicalize(dir.join("current")).ok();
    let Ok(entries) = fs::read_dir(dir.join("releases")) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let real = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let kept = current.as_deref() == Some(&real)
            || keep
                .iter()
                .any(|k| fs::canonicalize(k).ok().as_deref() == Some(&real));
        if !kept {
            let _ = fs::remove_dir_all(&path);
        }
    }
}

/// After an ordinary command of an installer copy: once the release in use has
/// run successfully, or a week after the switch, the release it replaced goes.
pub(crate) fn settle(succeeded: bool) {
    if let Ok(exe) = env::current_exe() {
        settle_at(&exe, succeeded, now());
    }
}

fn settle_at(exe: &Path, succeeded: bool, now: u64) {
    let Some(dir) = script_dir(exe) else {
        return; // not an installer copy, or an older process after a switch
    };
    let switched = fs::symlink_metadata(dir.join("current"))
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |t| t.as_secs());
    if succeeded || now.saturating_sub(switched) >= ROLLBACK_KEEP {
        prune_releases(&dir, &[]);
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

/// How this binary was installed, which decides how `update` replaces it.
#[derive(Clone, Debug, PartialEq)]
enum Kind {
    /// `cargo install --git … --tag vX`: Cargo owns `<root>/bin/rusty-mirror`.
    Cargo { cargo: PathBuf, root: PathBuf },
    /// scripts/install.sh: `<dir>/releases/<x.y.z>/` folders and a `<dir>/current`
    /// link. `base` is `RUSTY_MIRROR_RELEASE_BASE` (a local folder or URL, for tests).
    Script { dir: PathBuf, base: Option<String> },
}

/// Replaces this binary with a release, the way it was installed.
struct Installer {
    kind: Kind,
    /// Where the private build or download folder goes.
    temp: PathBuf,
}

impl Installer {
    fn locate() -> Result<Installer, String> {
        let exe = env::current_exe().map_err(|e| format!("cannot find this binary: {e}"))?;
        if let Some(dir) = script_dir(&exe) {
            let base = env::var("RUSTY_MIRROR_RELEASE_BASE")
                .ok()
                .filter(|b| !b.is_empty());
            return Ok(Installer {
                temp: dir.clone(), // same filesystem as the releases, so the move is a rename
                kind: Kind::Script { dir, base },
            });
        }
        let root = cargo_root(&exe).ok_or_else(|| {
            format!(
                "{} was not installed with cargo install or the installer",
                exe.display()
            )
        })?;
        let rustup = root.join("bin").join("cargo");
        Ok(Installer {
            kind: Kind::Cargo {
                cargo: if rustup.is_file() {
                    rustup
                } else {
                    "cargo".into()
                },
                root,
            },
            temp: env::temp_dir(),
        })
    }

    fn install(&self, version: &str, current: &str, progress: bool) -> Result<(), String> {
        let version = match parse_version(version) {
            Some(v) if Some(v) != parse_version(current) => format!("{}.{}.{}", v.0, v.1, v.2),
            _ => return Err(format!("refusing to install version {version:?}")),
        };
        match &self.kind {
            Kind::Cargo { cargo, root } => self.cargo_install(cargo, root, &version, progress),
            Kind::Script { dir, base } => {
                self.script_install(dir, base.as_deref(), &version, progress)
            }
        }
    }

    /// Cargo fetches the tag over HTTPS, builds exactly the tag's Cargo.lock
    /// (`--locked`, each crate checked against its recorded checksum) and only
    /// replaces the binary after a successful build.
    fn cargo_install(
        &self,
        cargo: &Path,
        root: &Path,
        version: &str,
        progress: bool,
    ) -> Result<(), String> {
        let tag = format!("v{version}");
        let build = BuildDir::create(&self.temp)?;
        let mut command = Command::new(cargo);
        command
            .args(["install", "--git", REPOSITORY, "--tag", &tag])
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
                .ok_or(format!("cargo install {tag} failed ({status})"));
        }
        let output = command.output().map_err(start)?;
        output
            .status
            .success()
            .then_some(())
            .ok_or_else(|| format!("cargo install {tag} failed: {}", tail(&output.stderr)))
    }

    /// Download the archive and SHA256SUMS into a private folder, refuse it
    /// unless the checksum matches, unpack it, check that it is `version` and
    /// runs, move it to `<dir>/releases/<version>` and switch `current` to it.
    /// The release that was current stays as the one rollback copy.
    fn script_install(
        &self,
        dir: &Path,
        base: Option<&str>,
        version: &str,
        progress: bool,
    ) -> Result<(), String> {
        let asset = platform_asset()?;
        let base = base.map_or_else(
            || {
                format!(
                    "https://github.com/{}/releases/download/v{version}",
                    github_slug()
                )
            },
            |b| b.trim_end_matches('/').to_owned(),
        );
        let build = BuildDir::create(&self.temp)?;
        if progress {
            eprintln!("Downloading {asset} ({version})...");
        }
        let (sums, archive) = (build.0.join("SHA256SUMS"), build.0.join(asset));
        download(&base, "SHA256SUMS", &sums)?;
        download(&base, asset, &archive)?;
        let listed = fs::read_to_string(&sums).map_err(|e| format!("SHA256SUMS: {e}"))?;
        let want = listed_sum(&listed, asset).ok_or(format!("no checksum for {asset}"))?;
        if sha256_file(&archive)? != want {
            return Err(format!(
                "checksum mismatch for {asset}; nothing was installed"
            ));
        }
        let unpacked = build.0.join("unpacked");
        fs::create_dir(&unpacked).map_err(|e| format!("{}: {e}", unpacked.display()))?;
        let tar = Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&unpacked)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("cannot run tar: {e}"))?;
        if !tar.status.success() {
            return Err(format!("cannot unpack {asset}: {}", tail(&tar.stderr)));
        }
        let payload = unpacked.join(TOOL);
        let stated = fs::read_to_string(payload.join("VERSION")).unwrap_or_default();
        if stated.trim() != version {
            return Err(format!("{asset} is not release {version}"));
        }
        let ran = Command::new(payload.join(TOOL))
            .arg("--version")
            .env("RUSTY_MIRROR_NO_UPDATE_CHECK", "1")
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("the downloaded CLI does not run here: {e}"))?;
        if !ran.status.success() || !String::from_utf8_lossy(&ran.stdout).contains(version) {
            return Err(format!("the downloaded CLI does not report {version}"));
        }
        let releases = dir.join("releases");
        let target = releases.join(version);
        let previous = fs::canonicalize(dir.join("current")).ok();
        if previous.is_some() && previous == fs::canonicalize(&target).ok() {
            return Err(format!("{version} is already the current release"));
        }
        let _ = fs::remove_dir_all(&target); // an older unpack of the same release
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&releases)
            .and_then(|()| fs::rename(&payload, &target))
            .map_err(|e| format!("{}: {e}", target.display()))?;
        swap_current(dir, &target)?;
        prune_releases(
            dir,
            &previous.iter().map(PathBuf::as_path).collect::<Vec<_>>(),
        );
        Ok(())
    }

    /// `update --rollback` for an installer copy: switch back to the kept
    /// folder when it is still there, else install that release again.
    fn rollback(&self, version: &str, current: &str, progress: bool) -> Result<(), String> {
        let Kind::Script { dir, .. } = &self.kind else {
            return self.install(version, current, progress); // Cargo keeps no copy
        };
        let version = match parse_version(version) {
            Some(v) if Some(v) != parse_version(current) => format!("{}.{}.{}", v.0, v.1, v.2),
            _ => return Err(format!("refusing to install version {version:?}")),
        };
        let version = version.as_str();
        let kept = dir.join("releases").join(version);
        let stated = fs::read_to_string(kept.join("VERSION")).unwrap_or_default();
        if stated.trim() != version || !kept.join(TOOL).is_file() {
            return self.install(version, current, progress);
        }
        let left = fs::canonicalize(dir.join("current")).ok();
        swap_current(dir, &kept)?;
        prune_releases(dir, &left.iter().map(PathBuf::as_path).collect::<Vec<_>>());
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
            let installer = Installer::locate().ok();
            check_report(&store, &api_url(), CURRENT, installer.map(|i| i.kind))
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
/// `command` names what `update` will run: itself for an installer copy, the
/// Cargo command otherwise.
fn check_report(
    store: &Store,
    url: &str,
    current: &str,
    kind: Option<Kind>,
) -> Result<(Value, u8), String> {
    let state = check(store, url)?;
    let new = available(&state, current);
    let command = new.map(|new| match kind {
        Some(Kind::Script { .. }) => format!("{TOOL} update"),
        _ => install_hint(kind.as_ref(), new),
    });
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
    let installer =
        installer.map_err(|e| format!("{e}; to update, run: {}", install_hint(None, &new)))?;
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

/// `update --rollback`: back to the version that ran before the last update.
/// An installer copy switches back to its kept folder; Cargo rebuilds the tag.
fn rollback(
    store: &Store,
    installer: Result<Installer, String>,
    current: &str,
    terminal: bool,
) -> Result<(Value, u8), String> {
    let kind = installer.as_ref().ok().map(|i| &i.kind);
    let previous = store.load().previous.ok_or_else(|| {
        format!(
            "no earlier version is recorded; to install one, run: {}",
            install_hint(kind, "X.Y.Z")
        )
    })?;
    installer?
        .rollback(&previous, current, terminal)
        .map_err(|e| format!("{e}; {current} is unchanged"))?;
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
                kind: Kind::Cargo { cargo, root },
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
        let Kind::Cargo { root, .. } = installer.kind.clone() else {
            unreachable!()
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

    /// A release folder as the archive unpacks: a CLI that reports `reports`
    /// and a VERSION file saying `stated`.
    fn payload(folder: &Path, stated: &str, reports: &str) {
        fs::create_dir_all(folder).unwrap();
        let cli = folder.join(TOOL);
        fs::write(&cli, format!("#!/bin/sh\necho 'rusty-mirror {reports}'\n")).unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(folder.join("VERSION"), format!("{stated}\n")).unwrap();
    }

    /// An installer copy of `version` (marker, release folder, `current` link)
    /// and an installer whose release base is the local folder `rel`.
    fn script_copy(dir: &TestDir, version: &str) -> (PathBuf, Installer) {
        let inst = dir.0.join("inst");
        fs::create_dir_all(&inst).unwrap();
        fs::write(inst.join(INSTALLER_MARKER), "").unwrap();
        let release = inst.join("releases").join(version);
        payload(&release, version, version);
        let _ = fs::remove_file(inst.join("current"));
        symlink(&release, inst.join("current")).unwrap();
        (inst.clone(), script_installer(dir, &inst))
    }

    fn script_installer(dir: &TestDir, inst: &Path) -> Installer {
        let base = dir.0.join("rel").to_string_lossy().into_owned();
        Installer {
            kind: Kind::Script {
                dir: inst.to_path_buf(),
                base: Some(base),
            },
            temp: inst.to_path_buf(),
        }
    }

    /// A local release folder in the layout scripts/build-release.sh publishes.
    fn publish(dir: &TestDir, stated: &str, reports: &str, good_sum: bool) {
        let (rel, stage) = (dir.0.join("rel"), dir.0.join("stage"));
        let _ = fs::remove_dir_all(&rel);
        let _ = fs::remove_dir_all(&stage);
        payload(&stage.join(TOOL), stated, reports);
        fs::create_dir_all(&rel).unwrap();
        let asset = platform_asset().unwrap();
        let tar = Command::new("tar")
            .arg("-czf")
            .arg(rel.join(asset))
            .arg("-C")
            .arg(&stage)
            .arg(TOOL)
            .status()
            .unwrap();
        assert!(tar.success());
        let sum = if good_sum {
            sha256_file(&rel.join(asset)).unwrap()
        } else {
            "0".repeat(64)
        };
        let sums = format!("{}  install.sh\n{sum}  {asset}\n", "1".repeat(64));
        fs::write(rel.join("SHA256SUMS"), sums).unwrap();
    }

    fn current(inst: &Path) -> String {
        let link = fs::read_link(inst.join("current")).unwrap();
        link.file_name().unwrap().to_string_lossy().into_owned()
    }

    /// Release folders, and anything else the updater left in the install folder.
    fn layout(inst: &Path) -> (Vec<String>, Vec<String>) {
        let names = |dir: PathBuf| {
            let mut names: Vec<String> = fs::read_dir(dir)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        };
        let extra = names(inst.to_path_buf())
            .into_iter()
            .filter(|n| !["releases", "current", INSTALLER_MARKER].contains(&n.as_str()))
            .collect();
        (names(inst.join("releases")), extra)
    }

    #[test]
    fn installer_copies_are_detected_only_while_current() {
        let dir = TestDir::new("script-detect");
        let (inst, installer) = script_copy(&dir, "0.3.0");
        let real = fs::canonicalize(&inst).unwrap();
        let exe = inst.join("releases/0.3.0").join(TOOL);
        assert_eq!(script_dir(&exe), Some(real.clone()));
        assert_eq!(script_dir(&inst.join("current").join(TOOL)), Some(real));
        payload(&inst.join("releases/0.2.0"), "0.2.0", "0.2.0");
        assert_eq!(
            script_dir(&inst.join("releases/0.2.0").join(TOOL)),
            None,
            "not the current release"
        );
        let hint = install_hint(Some(&installer.kind), "0.4.0");
        assert!(
            hint.contains("/releases/latest/download/install.sh"),
            "{hint}"
        );
        assert!(hint.contains("RUSTY_MIRROR_VERSION=0.4.0 sh"), "{hint}");
        fs::remove_file(inst.join(INSTALLER_MARKER)).unwrap();
        assert_eq!(script_dir(&exe), None, "no installer marker");
        assert_eq!(script_dir(&dir.0.join(TOOL)), None);
    }

    #[test]
    fn installer_update_verifies_then_swaps_and_keeps_one_previous() {
        let dir = TestDir::new("script-update");
        let (inst, installer) = script_copy(&dir, "0.3.0");
        payload(&inst.join("releases/0.2.0"), "0.2.0", "0.2.0");
        publish(&dir, "0.4.0", "0.4.0", true);
        installer.install("v0.4.0", "0.3.0", false).unwrap();
        assert_eq!(current(&inst), "0.4.0");
        let ran = Command::new(inst.join("current").join(TOOL))
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8_lossy(&ran.stdout).trim(),
            "rusty-mirror 0.4.0"
        );
        assert_eq!(
            layout(&inst),
            (vec!["0.3.0".into(), "0.4.0".into()], vec![]),
            "one rollback copy, no download folder"
        );
    }

    #[test]
    fn installer_update_refuses_a_bad_checksum_version_or_download() {
        let dir = TestDir::new("script-refuse");
        let (inst, installer) = script_copy(&dir, "0.3.0");
        let unchanged = (vec!["0.3.0".to_string()], vec![]);
        publish(&dir, "0.4.0", "0.4.0", false);
        let error = installer.install("0.4.0", "0.3.0", false).unwrap_err();
        assert!(error.contains("checksum mismatch"), "{error}");
        assert_eq!(
            (current(&inst), layout(&inst)),
            ("0.3.0".into(), unchanged.clone())
        );
        publish(&dir, "0.5.0", "0.5.0", true);
        let error = installer.install("0.4.0", "0.3.0", false).unwrap_err();
        assert!(error.contains("is not release 0.4.0"), "{error}");
        assert_eq!(
            (current(&inst), layout(&inst)),
            ("0.3.0".into(), unchanged.clone())
        );
        publish(&dir, "0.4.0", "0.3.9", true);
        let error = installer.install("0.4.0", "0.3.0", false).unwrap_err();
        assert!(error.contains("does not report 0.4.0"), "{error}");
        fs::remove_dir_all(dir.0.join("rel")).unwrap();
        let error = installer.install("0.4.0", "0.3.0", false).unwrap_err();
        assert!(error.contains("cannot copy SHA256SUMS"), "{error}");
        assert_eq!((current(&inst), layout(&inst)), ("0.3.0".into(), unchanged));
    }

    #[test]
    fn installer_rollback_switches_back_or_reinstalls_the_release() {
        let dir = TestDir::new("script-rollback");
        let (inst, installer) = script_copy(&dir, "0.3.0");
        publish(&dir, "0.4.0", "0.4.0", true);
        installer.install("0.4.0", "0.3.0", false).unwrap();
        let store = dir.store();
        store
            .save(&State {
                previous: Some("0.3.0".into()),
                ..State::default()
            })
            .unwrap();
        fs::remove_dir_all(dir.0.join("rel")).unwrap(); // the kept copy needs no download
        let (report, _) = rollback(&store, Ok(installer), "0.4.0", false).unwrap();
        assert_eq!(report["installed"], json!("0.3.0"));
        assert_eq!(current(&inst), "0.3.0");
        assert_eq!(
            layout(&inst).0,
            ["0.3.0", "0.4.0"],
            "the version left is kept"
        );

        prune_releases(&inst, &[]);
        publish(&dir, "0.4.0", "0.4.0", true);
        script_installer(&dir, &inst)
            .rollback("0.4.0", "0.3.0", false)
            .unwrap();
        assert_eq!(current(&inst), "0.4.0", "no kept copy: verified download");
        assert_eq!(
            layout(&inst),
            (vec!["0.3.0".into(), "0.4.0".into()], vec![])
        );
    }

    #[test]
    fn replaced_release_goes_after_one_good_run_or_a_week() {
        let dir = TestDir::new("script-settle");
        let (inst, installer) = script_copy(&dir, "0.3.0");
        publish(&dir, "0.4.0", "0.4.0", true);
        installer.install("0.4.0", "0.3.0", false).unwrap();
        let (old, new) = (
            inst.join("releases/0.3.0").join(TOOL),
            inst.join("current").join(TOOL),
        );
        settle_at(&old, true, now());
        assert_eq!(layout(&inst).0.len(), 2, "an older process removes nothing");
        settle_at(&new, false, now());
        assert_eq!(layout(&inst).0.len(), 2, "kept until the new one has run");
        settle_at(&new, false, now() + ROLLBACK_KEEP);
        assert_eq!(layout(&inst).0, ["0.4.0"], "at most a week");

        let other = TestDir::new("script-settle-ok");
        let (inst, _) = script_copy(&other, "0.3.0");
        let installer = Installer {
            kind: Kind::Script {
                dir: inst.clone(),
                base: Some(dir.0.join("rel").to_string_lossy().into_owned()),
            },
            temp: inst.clone(),
        };
        installer.install("0.4.0", "0.3.0", false).unwrap();
        settle_at(&inst.join("current").join(TOOL), true, now());
        assert_eq!(layout(&inst).0, ["0.4.0"], "one good run");
    }

    #[test]
    fn sha256sums_lines_are_matched_by_exact_name() {
        let sums = format!(
            "{a}  rusty-mirror-linux-x86_64.tar.gz\n{b} *rusty-mirror-darwin-universal.tar.gz\nbad  install.sh\n",
            a = "a".repeat(64),
            b = "B".repeat(64)
        );
        assert_eq!(
            listed_sum(&sums, "rusty-mirror-darwin-universal.tar.gz"),
            Some("b".repeat(64))
        );
        assert_eq!(
            listed_sum(&sums, "rusty-mirror-linux-x86_64.tar.gz"),
            Some("a".repeat(64))
        );
        assert_eq!(listed_sum(&sums, "install.sh"), None, "not a SHA-256");
        assert_eq!(listed_sum(&sums, "rusty-mirror-linux"), None);
    }
}
