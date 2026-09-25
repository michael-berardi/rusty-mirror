//! Immutable frontend revisions. `stage` copies a built frontend into
//! `<app>/hot-assets/<sha256>/` without contacting the app; `load` reads one
//! back, re-verifying every byte, so the app never serves a file it has not
//! checked. A revision is never edited in place: change the code, stage again.
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

pub const MAX_FILE: u64 = 16 * 1024 * 1024;
pub const MAX_TOTAL: u64 = 64 * 1024 * 1024;
pub const MAX_FILES: usize = 4096;
const MANIFEST: &str = "manifest.json";

pub type Files = BTreeMap<String, Vec<u8>>;

#[derive(Debug)]
pub struct Staged {
    pub revision: String,
    pub path: std::path::PathBuf,
    pub files: usize,
    pub bytes: u64,
    pub already_staged: bool,
}

fn safe_component(c: &str) -> bool {
    !c.is_empty()
        && c != "."
        && c != ".."
        && !c.starts_with('.')
        && c.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_' | b'@' | b'+'))
}

fn private_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [".pem", ".key", ".p12", ".pfx", ".env"]
        .iter()
        .any(|s| lower.ends_with(s))
        || lower.starts_with(".env")
}

/// Read and validate a built frontend directory.
pub fn read_dist(dist: &Path) -> Result<Files, String> {
    // The build directory itself must be real; system-level ancestors such as
    // macOS's /var -> /private/var are resolved rather than refused.
    if std::fs::symlink_metadata(dist)
        .map_err(|e| format!("{}: {e}", dist.display()))?
        .file_type()
        .is_symlink()
    {
        return Err(format!(
            "{} is a symlink; pass the real build directory",
            dist.display()
        ));
    }
    let dist = dist.canonicalize().map_err(|e| e.to_string())?;
    let mut files = Files::new();
    let mut total = 0u64;
    let mut stack = vec![dist.clone()];
    while let Some(dir) = stack.pop() {
        let mut entries: Vec<_> = std::fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        entries.sort_by_key(|e| e.file_name());
        for entry in entries {
            let path = entry.path();
            let name = entry
                .file_name()
                .into_string()
                .map_err(|_| "non-UTF-8 file name")?;
            if private_name(&name) {
                return Err(format!("refusing private-looking file {}", path.display()));
            }
            if !safe_component(&name) {
                return Err(format!("unsafe or hidden name {}", path.display()));
            }
            let m = std::fs::symlink_metadata(&path).map_err(|e| e.to_string())?;
            if m.file_type().is_symlink() {
                return Err(format!("symlink in build: {}", path.display()));
            }
            if m.is_dir() {
                stack.push(path);
                continue;
            }
            if !m.is_file() || m.nlink() != 1 {
                return Err(format!("not a plain single-link file: {}", path.display()));
            }
            if m.len() > MAX_FILE {
                return Err(format!("{} is over 16 MiB", path.display()));
            }
            total += m.len();
            let key = path
                .strip_prefix(&dist)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if key == MANIFEST || key.len() > 240 {
                return Err(format!("reserved or overlong path {key}"));
            }
            if total > MAX_TOTAL || files.len() >= MAX_FILES {
                return Err("build exceeds 64 MiB / 4096 files".into());
            }
            files.insert(key, std::fs::read(&path).map_err(|e| e.to_string())?);
        }
    }
    check_index(&files)?;
    Ok(files)
}

/// `index.html` must load its module script(s) through relative, staged paths,
/// and the bundle must contain the snapshot hook, i.e. a mirror-debug build.
fn check_index(files: &Files) -> Result<(), String> {
    let index = files.get("index.html").ok_or("missing index.html")?;
    let html = String::from_utf8_lossy(index);
    let mut module_scripts = 0;
    for (attr, value) in attributes(&html) {
        let value = value.split(['?', '#']).next().unwrap_or("");
        if value.is_empty() || value.starts_with("data:") {
            continue;
        }
        let local = value.strip_prefix("./").unwrap_or(value);
        if value.starts_with('/') || value.contains("://") || value.starts_with("//") {
            return Err(format!(
                "index.html {attr}=\"{value}\" is not relative; build with a relative base (e.g. vite --base ./)"
            ));
        }
        if !files.contains_key(local) {
            return Err(format!(
                "index.html references {value}, which is not in the build"
            ));
        }
        if attr == "src" && local.ends_with(".js") {
            module_scripts += 1;
        }
    }
    if module_scripts == 0 {
        return Err("index.html loads no local script".into());
    }
    let marker = crate::SNAPSHOT_GLOBAL.as_bytes();
    if !files
        .iter()
        .any(|(k, v)| (k.ends_with(".js") || k.ends_with(".mjs")) && contains(v, marker))
    {
        return Err(format!(
            "no script defines {}; this is a consumer build. Rebuild the frontend with the mirror gate on",
            crate::SNAPSHOT_GLOBAL
        ));
    }
    Ok(())
}

/// Tiny attribute scanner: enough for src/href in generated index.html files.
fn attributes(html: &str) -> Vec<(&'static str, &str)> {
    let mut out = Vec::new();
    for attr in ["src", "href"] {
        for quote in ['"', '\''] {
            let needle = format!(" {attr}={quote}");
            let mut rest = html;
            while let Some(i) = rest.find(&needle) {
                rest = &rest[i + needle.len()..];
                if let Some(end) = rest.find(quote) {
                    out.push((attr, &rest[..end]));
                    rest = &rest[end..];
                }
            }
        }
    }
    out
}

pub fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

fn sha256(data: &[u8]) -> String {
    let digest = Sha256::digest(data);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn manifest(files: &Files) -> (String, Vec<u8>) {
    let specs: BTreeMap<&str, serde_json::Value> = files
        .iter()
        .map(|(k, v)| {
            (
                k.as_str(),
                serde_json::json!({"size": v.len(), "sha256": sha256(v)}),
            )
        })
        .collect();
    let revision = sha256(&serde_json::to_vec(&specs).unwrap());
    let body =
        serde_json::to_vec_pretty(&serde_json::json!({"revision": revision, "files": specs}))
            .unwrap();
    (revision, body)
}

/// Stage `dist` for `app`. Staging the same bytes twice is a no-op.
pub fn stage(app: &str, dist: &Path) -> Result<Staged, String> {
    let files = read_dist(dist)?;
    let (revision, manifest) = manifest(&files);
    let root = crate::paths::hot_dir(app)?;
    crate::paths::ensure_private_dir(root.parent().unwrap())?;
    crate::paths::ensure_private_dir(&root)?;
    let target = root.join(&revision);
    let bytes = files.values().map(|v| v.len() as u64).sum();
    if target.exists() {
        load(app, &revision)?; // prove the existing copy is intact
        return Ok(Staged {
            revision,
            path: target,
            files: files.len(),
            bytes,
            already_staged: true,
        });
    }
    let temp = root.join(format!(".stage-{}-{}", std::process::id(), &revision[..12]));
    let result = (|| {
        std::fs::DirBuilder::new()
            .mode_private()
            .create(&temp)
            .map_err(|e| e.to_string())?;
        for (key, data) in files
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_slice()))
            .chain([(MANIFEST, manifest.as_slice())])
        {
            let path = temp.join(key);
            if let Some(parent) = path.parent() {
                std::fs::DirBuilder::new()
                    .recursive(true)
                    .mode_private()
                    .create(parent)
                    .map_err(|e| e.to_string())?;
            }
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            f.write_all(data)
                .and_then(|_| f.sync_all())
                .map_err(|e| e.to_string())?;
        }
        std::fs::rename(&temp, &target).map_err(|e| e.to_string())
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir_all(&temp);
    }
    result?;
    Ok(Staged {
        revision,
        path: target,
        files: files.len(),
        bytes,
        already_staged: false,
    })
}

trait PrivateDir {
    fn mode_private(&mut self) -> &mut Self;
}
impl PrivateDir for std::fs::DirBuilder {
    fn mode_private(&mut self) -> &mut Self {
        use std::os::unix::fs::DirBuilderExt;
        self.mode(0o700)
    }
}

/// Load a staged revision into memory, verifying ownership, modes and hashes.
pub fn load(app: &str, revision: &str) -> Result<Files, String> {
    if !crate::protocol::is_revision(revision) {
        return Err("bad revision".into());
    }
    let root = crate::paths::hot_dir(app)?;
    crate::paths::check_private_dir(&root)?;
    let dir = root.join(revision);
    crate::paths::check_private_dir(&dir)?;
    let read = |key: &str| -> Result<Vec<u8>, String> {
        if !key.split('/').all(safe_component) {
            return Err(format!("unsafe manifest path {key}"));
        }
        let path = dir.join(key);
        let m = std::fs::symlink_metadata(&path).map_err(|e| format!("{key}: {e}"))?;
        if !m.is_file()
            || m.uid() != crate::paths::euid()
            || m.mode() & 0o077 != 0
            || m.len() > MAX_FILE
        {
            return Err(format!("staged file {key} is not a private regular file"));
        }
        std::fs::read(path).map_err(|e| e.to_string())
    };
    let manifest: serde_json::Value =
        serde_json::from_slice(&read(MANIFEST)?).map_err(|e| format!("manifest: {e}"))?;
    let specs = manifest["files"]
        .as_object()
        .ok_or("manifest has no files")?;
    let mut files = Files::new();
    for (key, spec) in specs {
        let data = read(key)?;
        if Some(data.len() as u64) != spec["size"].as_u64()
            || Some(sha256(&data).as_str()) != spec["sha256"].as_str()
        {
            return Err(format!("staged file {key} does not match its manifest"));
        }
        files.insert(key.clone(), data);
    }
    let (actual, _) = self::manifest(&files);
    if actual != revision || manifest["revision"].as_str() != Some(revision) {
        return Err("staged revision digest mismatch".into());
    }
    check_index(&files)?;
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> std::path::PathBuf {
        let p =
            std::env::temp_dir().join(format!("rusty-mirror-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn write(dist: &Path, key: &str, body: &str) {
        let p = dist.join(key);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }

    #[test]
    fn stages_verifies_and_refuses_consumer_builds() {
        let base = temp("stage");
        std::env::set_var("RUSTY_MIRROR_HOME", base.join("home"));
        let dist = base.join("dist");
        write(
            &dist,
            "index.html",
            r#"<script type="module" src="./assets/app.js"></script><link rel="stylesheet" href="assets/app.css">"#,
        );
        write(&dist, "assets/app.css", "body{}");
        write(&dist, "assets/app.js", "console.log('consumer')");
        let err = stage("t.app", &dist).unwrap_err();
        assert!(err.contains("consumer build"), "{err}");

        write(
            &dist,
            "assets/app.js",
            "window.__RUSTY_MIRROR_SNAPSHOT__ = () => ({})",
        );
        let first = stage("t.app", &dist).unwrap();
        assert!(!first.already_staged);
        assert_eq!(load("t.app", &first.revision).unwrap().len(), 3);
        assert!(stage("t.app", &dist).unwrap().already_staged);

        // Tampering is caught on load.
        std::fs::write(first.path.join("assets/app.css"), "body{color:red}").unwrap();
        assert!(load("t.app", &first.revision)
            .unwrap_err()
            .contains("manifest"));

        write(
            &dist,
            "index.html",
            r#"<script type="module" src="/assets/app.js"></script>"#,
        );
        assert!(stage("t.app", &dist).unwrap_err().contains("not relative"));
        write(
            &dist,
            "index.html",
            r#"<script type="module" src="./assets/app.js"></script>"#,
        );
        write(&dist, ".env", "SECRET=1");
        assert!(stage("t.app", &dist).is_err());
        std::fs::remove_dir_all(base).unwrap();
    }
}
