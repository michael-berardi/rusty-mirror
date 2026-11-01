//! Neutrality test modelled on Carl's test. Rusty Mirror ships no built-in
//! model, provider, fallback choice or personal/project name: those belong in
//! the operator's own configuration. This test keeps it that way.
//!
//! Excluded from the scan:
//! - `Cargo.toml` / `LICENSE`: package metadata author fields and legal attribution.
//! - `tests/`: synthetic fixtures and test data.
//! - `examples/tally/dist/`: generated build output (gitignored).
//! - `examples/tally/src-tauri/tauri.conf.json`: product technical identity
//!   (bundle identifier `dev.rustymirror.tally`), explicitly allowlisted.

use std::fs;
use std::path::{Path, PathBuf};

/// Concrete model ids and provider preferences that only an operator should choose.
const MODEL_PATTERNS: &[&str] = &[
    "gpt-5", "gpt-6", "deepseek", "opencode-go/", "openrouter/", ":exacto", "mimo-v", "glm-",
    "claude-", "haiku", "sonnet", "opus",
];

/// Names of the people and organisations Rusty Mirror was first built for: never in the shipped tool.
const PERSONAL_PATTERNS: &[&str] = &[
    "Mike",
    "Berardi",
    "Kate",
    "Felix",
    "Lucero",
    "PlataPlace",
    "Mouseia",
    "Liberty Design",
    "libertydesign",
    "UltraLDSBot",
    "/Users/",
];

/// Directories to skip entirely.
const SKIP_DIRS: &[&str] = &[".git", "target", "dist", "node_modules", "tests"];

/// Files to skip entirely.
const SKIP_FILES: &[&str] = &[
    "Cargo.toml", // Package metadata: author, repository, homepage.
    "LICENSE",    // Legal attribution.
    "Cargo.lock", // Generated lockfile.
];

/// Files explicitly allowlisted as product technical identity.
const ALLOWLIST: &[&str] = &[
    "examples/tally/src-tauri/tauri.conf.json", // Bundle identifier dev.rustymirror.tally.
];

fn should_skip(path: &Path) -> bool {
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    let path_str = path.to_string_lossy();

    if path.is_dir() && SKIP_DIRS.contains(&file_name) {
        return true;
    }
    if SKIP_FILES.contains(&file_name) {
        return true;
    }
    if path_str.contains("tests/neutrality.rs") {
        return true;
    }
    for allow in ALLOWLIST {
        if path_str.ends_with(allow) {
            return true;
        }
    }
    false
}

fn scan_dir(dir: &Path, hits: &mut Vec<String>) {
    if !dir.exists() {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries {
        let path = match entry {
            Ok(e) => e.path(),
            Err(_) => continue,
        };
        if should_skip(&path) {
            continue;
        }
        if path.is_dir() {
            scan_dir(&path, hits);
        } else if path.is_file() {
            scan_file(&path, hits);
        }
    }
}

fn scan_file(path: &Path, hits: &mut Vec<String>) {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return, // Skip binary or unreadable files.
    };
    for (i, line) in content.lines().enumerate() {
        for pattern in MODEL_PATTERNS {
            if line.contains(pattern) {
                hits.push(format!(
                    "{}:{}: [model] {}",
                    path.display(),
                    i + 1,
                    line.trim()
                ));
            }
        }
        for pattern in PERSONAL_PATTERNS {
            if line.contains(pattern) {
                hits.push(format!(
                    "{}:{}: [personal] {}",
                    path.display(),
                    i + 1,
                    line.trim()
                ));
            }
        }
    }
}

#[test]
fn shipped_files_are_provider_and_person_neutral() {
    // The workspace root is two levels above the crate manifest dir.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir.parent().unwrap().parent().unwrap();

    let targets: Vec<PathBuf> = vec![
        root.join("crates"),
        root.join("docs"),
        root.join("skills"),
        root.join("examples"),
        root.join("scripts"),
        root.join("README.md"),
        root.join("SECURITY.md"),
    ];

    let mut hits = Vec::new();
    for target in targets {
        if target.is_dir() {
            scan_dir(&target, &mut hits);
        } else if target.is_file() {
            scan_file(&target, &mut hits);
        }
    }

    if !hits.is_empty() {
        let mut message =
            String::from("Neutrality scan found forbidden strings in shipped files:\n");
        for hit in &hits {
            message.push_str(hit);
            message.push('\n');
        }
        panic!("{}", message);
    }
}
