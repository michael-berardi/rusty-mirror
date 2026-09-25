//! Proof that a consumer build does not carry the mirror, and that no Tauri
//! capability quietly hands plugin permissions to the mirror window.
use std::path::Path;

/// Strings only a mirror-enabled build contains (frontend or native).
pub const MARKERS: &[&str] = &[
    crate::SNAPSHOT_GLOBAL,
    crate::SEED_GLOBAL,
    crate::QUERY,
    crate::HOT_PREFIX,
];

#[derive(Debug, serde::Serialize)]
pub struct Finding {
    pub path: String,
    pub marker: String,
}

/// Scan a file or directory tree (frontend dist, app binary, .app bundle).
pub fn scan(path: &Path) -> Result<(Vec<Finding>, usize), String> {
    let mut findings = Vec::new();
    let mut scanned = 0usize;
    let mut stack = vec![path.to_path_buf()];
    while let Some(p) = stack.pop() {
        let m = std::fs::symlink_metadata(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        if m.is_dir() {
            for entry in std::fs::read_dir(&p).map_err(|e| e.to_string())? {
                stack.push(entry.map_err(|e| e.to_string())?.path());
            }
        } else if m.is_file() {
            scanned += 1;
            let data = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
            for marker in MARKERS {
                if crate::stage::contains(&data, marker.as_bytes()) {
                    findings.push(Finding {
                        path: p.display().to_string(),
                        marker: marker.to_string(),
                    });
                }
            }
        }
    }
    Ok((findings, scanned))
}

/// Tauri-style window glob: `*` any run, `?` one character.
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    fn go(p: &[u8], t: &[u8]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (Some(b'*'), _) => go(&p[1..], t) || (!t.is_empty() && go(p, &t[1..])),
            (Some(b'?'), Some(_)) => go(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a == b => go(&p[1..], &t[1..]),
            _ => false,
        }
    }
    go(pattern.as_bytes(), text.as_bytes())
}

/// Capability files (JSON) whose `windows`/`webviews` would include the mirror.
pub fn capabilities(dir: &Path) -> Result<(Vec<Finding>, usize), String> {
    let mut findings = Vec::new();
    let mut scanned = 0;
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    entries.sort();
    for path in entries {
        scanned += 1;
        let value: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).map_err(|e| e.to_string())?)
                .map_err(|e| format!("{}: {e}", path.display()))?;
        let caps = match &value {
            serde_json::Value::Array(list) => list.clone(),
            serde_json::Value::Object(o) if o.contains_key("capabilities") => {
                o["capabilities"].as_array().cloned().unwrap_or_default()
            }
            other => vec![other.clone()],
        };
        for cap in caps {
            for key in ["windows", "webviews"] {
                for pattern in cap[key]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str())
                {
                    if glob_matches(pattern, crate::LABEL) {
                        findings.push(Finding {
                            path: path.display().to_string(),
                            marker: format!("{key} pattern {pattern:?} grants the mirror window plugin permissions"),
                        });
                    }
                }
            }
        }
    }
    Ok((findings, scanned))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob_matches("*", "rusty-mirror"));
        assert!(glob_matches("rusty-*", "rusty-mirror"));
        assert!(glob_matches("rusty-mirro?", "rusty-mirror"));
        assert!(!glob_matches("main", "rusty-mirror"));
        assert!(!glob_matches("main*", "rusty-mirror"));
    }
}
