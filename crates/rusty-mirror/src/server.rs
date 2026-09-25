//! Same-user unix socket. One request per connection, handled in order.
use rusty_mirror_core::{paths, protocol};
use serde_json::json;
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    sync::Mutex,
    time::Duration,
};
use tauri::{AppHandle, Wry};

/// (path, inode) of the socket this process created, so cleanup never removes
/// a socket that belongs to somebody else.
static OWNED: Mutex<Option<(PathBuf, u64)>> = Mutex::new(None);

pub(crate) fn start(app: AppHandle<Wry>) -> Result<(), String> {
    let config = crate::config()?;
    let path = paths::socket_path(&config.app_id)?;
    let dir = path.parent().unwrap();
    paths::ensure_private_dir(&paths::root()?)?;
    paths::ensure_private_dir(dir)?;
    if let Ok(meta) = std::fs::symlink_metadata(&path) {
        use std::os::unix::fs::FileTypeExt;
        if UnixStream::connect(&path).is_ok() {
            return Err(format!(
                "{} is live; another instance owns it",
                path.display()
            ));
        }
        if !meta.file_type().is_socket() || meta.uid() != paths::euid() {
            return Err(format!(
                "{} exists and is not our stale socket; leaving it alone",
                path.display()
            ));
        }
        std::fs::remove_file(&path).map_err(|e| e.to_string())?; // ours, and nobody is listening
    }
    let listener =
        UnixListener::bind(&path).map_err(|e| format!("bind {}: {e}", path.display()))?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;
    let inode = std::fs::symlink_metadata(&path)
        .map_err(|e| e.to_string())?
        .ino();
    *OWNED.lock().unwrap_or_else(|p| p.into_inner()) = Some((path, inode));
    std::thread::Builder::new()
        .name("rusty-mirror-control".into())
        .spawn(move || {
            for stream in listener.incoming().flatten() {
                serve(&app, stream);
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn peer_uid(stream: &UnixStream) -> Option<u32> {
    use std::os::fd::AsRawFd;
    #[cfg(any(
        target_os = "macos",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    unsafe {
        let (mut uid, mut gid) = (0, 0);
        (libc::getpeereid(stream.as_raw_fd(), &mut uid, &mut gid) == 0).then_some(uid)
    }
    #[cfg(target_os = "linux")]
    unsafe {
        let mut cred: libc::ucred = std::mem::zeroed();
        let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
        (libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&mut cred as *mut libc::ucred).cast(),
            &mut len,
        ) == 0)
            .then_some(cred.uid)
    }
}

fn serve(app: &AppHandle<Wry>, stream: UnixStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(30)));
    let response = if peer_uid(&stream) != Some(paths::euid()) {
        json!({"ok": false, "error": "peer is not the same user"})
    } else {
        let mut line = Vec::new();
        let read = BufReader::new(
            stream
                .try_clone()
                .expect("socket clone")
                .take(protocol::MAX_REQUEST as u64 + 1),
        )
        .read_until(b'\n', &mut line);
        match read
            .map_err(|e| e.to_string())
            .and_then(|_| protocol::decode_request(line.trim_ascii_end()))
        {
            Err(error) => json!({"ok": false, "error": error}),
            Ok(request) => match crate::window::handle(app, request) {
                Ok(result) => json!({"ok": true, "result": result}),
                Err(error) => json!({"ok": false, "error": error}),
            },
        }
    };
    let mut out = serde_json::to_vec(&response)
        .unwrap_or_else(|_| br#"{"ok":false,"error":"encode"}"#.to_vec());
    out.push(b'\n');
    let mut stream = stream;
    let _ = stream.write_all(&out);
}

pub(crate) fn cleanup() {
    if let Some((path, inode)) = OWNED.lock().unwrap_or_else(|p| p.into_inner()).take() {
        if std::fs::symlink_metadata(&path).is_ok_and(|m| m.ino() == inode) {
            let _ = std::fs::remove_file(&path);
        }
    }
}
