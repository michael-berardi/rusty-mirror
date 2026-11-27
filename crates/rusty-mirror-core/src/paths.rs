//! Filesystem layout. Everything lives under one private root:
//!
//! ```text
//! $RUSTY_MIRROR_HOME (default ~/.rusty-mirror)
//! └── <app-id>/
//!     ├── control.sock      0600 unix socket, owned by the running app
//!     └── hot-assets/<rev>/ immutable staged frontend revisions
//! ```
use std::{
    io,
    os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt},
    path::{Path, PathBuf},
};

/// App ids become directory names, so they are deliberately boring.
pub fn validate_app_id(app: &str) -> Result<(), String> {
    let ok = !app.is_empty()
        && app.len() <= 128
        && !app.starts_with('.')
        && app
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    if ok {
        Ok(())
    } else {
        Err(format!(
            "app id {app:?} must be 1-128 characters of [A-Za-z0-9._-] and not start with '.'"
        ))
    }
}

/// `$RUSTY_MIRROR_HOME`, else `$HOME/.rusty-mirror`. Must be absolute.
pub fn root() -> Result<PathBuf, String> {
    let path = match std::env::var_os("RUSTY_MIRROR_HOME") {
        Some(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            let home = std::env::var_os("HOME").ok_or("HOME is not set")?;
            PathBuf::from(home).join(".rusty-mirror")
        }
    };
    if !path.is_absolute() || path.components().any(|c| c.as_os_str() == "..") {
        return Err("RUSTY_MIRROR_HOME must be an absolute path without '..'".into());
    }
    Ok(path)
}

pub fn app_dir(app: &str) -> Result<PathBuf, String> {
    validate_app_id(app)?;
    Ok(root()?.join(app))
}

pub fn socket_path(app: &str) -> Result<PathBuf, String> {
    let path = app_dir(app)?.join("control.sock");
    // sockaddr_un holds ~104 bytes on macOS (108 on Linux), terminator included.
    if path.as_os_str().len() > 103 {
        return Err(format!(
            "socket path is {} bytes, over the unix limit of 103: {}. Set RUSTY_MIRROR_HOME to a shorter directory",
            path.as_os_str().len(),
            path.display()
        ));
    }
    Ok(path)
}

pub fn hot_dir(app: &str) -> Result<PathBuf, String> {
    Ok(app_dir(app)?.join("hot-assets"))
}

/// Create (mode 0700) or accept a directory that is ours, real, and private.
pub fn ensure_private_dir(path: &Path) -> Result<(), String> {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)
                .map_err(|e| format!("create {}: {e}", path.display()))?;
            // `recursive` applies the umask to parents; tighten the leaf explicitly.
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        Err(e) => return Err(format!("{}: {e}", path.display())),
        Ok(_) => {}
    }
    check_private_dir(path)
}

/// A directory that is not a symlink, is owned by us, and has no group/other bits.
pub fn check_private_dir(path: &Path) -> Result<(), String> {
    let m = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    if !m.file_type().is_dir() {
        return Err(format!("{} is not a real directory", path.display()));
    }
    if m.uid() != euid() {
        return Err(format!(
            "{} is not owned by the current user",
            path.display()
        ));
    }
    if m.mode() & 0o077 != 0 {
        return Err(format!(
            "{} must not be accessible to group/other (chmod 700)",
            path.display()
        ));
    }
    Ok(())
}

/// A socket we can trust enough to talk to: ours, a socket, mode 0600-ish.
pub fn check_socket(path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        check_private_dir(parent)?;
    }
    let m = std::fs::symlink_metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
    use std::os::unix::fs::FileTypeExt;
    if !m.file_type().is_socket() || m.uid() != euid() || m.mode() & 0o077 != 0 {
        return Err(format!(
            "{} is not a private socket owned by you",
            path.display()
        ));
    }
    Ok(())
}

/// Create any missing parent directories of a stored file with mode 0700.
/// Directories that already exist (e.g. `$HOME`, `/tmp` or the current
/// project) are left as they are; the file itself is written 0600.
pub fn ensure_private_parent(file: &Path) -> Result<(), String> {
    let Some(parent) = file.parent().filter(|p| !p.as_os_str().is_empty()) else {
        return Ok(());
    };
    let mut missing = Vec::new();
    let mut current = parent;
    loop {
        match std::fs::symlink_metadata(current) {
            Ok(_) => break,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                missing.push(current.to_path_buf());
                match current.parent() {
                    Some(next) if !next.as_os_str().is_empty() => current = next,
                    _ => break,
                }
            }
            Err(e) => return Err(format!("{}: {e}", current.display())),
        }
    }
    for dir in missing.into_iter().rev() {
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&dir)
            .map_err(|e| format!("create {}: {e}", dir.display()))?;
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    let m = std::fs::metadata(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    if !m.is_dir() {
        return Err(format!("{} is not a directory", parent.display()));
    }
    Ok(())
}

pub fn euid() -> u32 {
    // SAFETY: geteuid has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Apps that currently have a control socket, for `rusty-mirror` auto-discovery.
pub fn known_apps() -> Vec<String> {
    let Ok(root) = root() else { return vec![] };
    let Ok(entries) = std::fs::read_dir(root) else {
        return vec![];
    };
    let mut apps: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|name| validate_app_id(name).is_ok())
        .filter(|name| socket_path(name).map(|p| p.exists()).unwrap_or(false))
        .collect();
    apps.sort();
    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids_are_boring() {
        assert!(validate_app_id("com.example.tally").is_ok());
        assert!(validate_app_id("my_app-2").is_ok());
        for bad in ["", ".hidden", "../up", "a/b", "sp ace", &"x".repeat(129)] {
            assert!(validate_app_id(bad).is_err(), "{bad:?} accepted");
        }
    }

    #[test]
    fn stored_file_parents_are_private() {
        let base = std::env::temp_dir().join(format!(
            "rusty-mirror-parent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let file = base.join("deep/nested/shot.png");
        ensure_private_parent(&file).unwrap();
        for dir in [base.join("deep"), base.join("deep/nested")] {
            let mode = std::fs::symlink_metadata(&dir).unwrap().mode();
            assert_eq!(mode & 0o777, 0o700, "{}", dir.display());
        }

        let public = base.join("public");
        std::fs::create_dir(&public).unwrap();
        std::fs::set_permissions(&public, std::fs::Permissions::from_mode(0o755)).unwrap();
        // An existing, user-chosen folder is accepted unchanged.
        ensure_private_parent(&public.join("shot.png")).unwrap();
        let mode = std::fs::symlink_metadata(&public).unwrap().mode();
        assert_eq!(mode & 0o777, 0o755);
        std::fs::remove_dir_all(base).unwrap();
    }
}
