//! Neutrality test modelled on Carl's test. Rusty Mirror ships no built-in
//! model, provider, fallback choice or personal/project name: those belong in
//! the operator's own configuration. This test keeps it that way.
//!
//! Excluded from the scan:
//! - `Cargo.toml` / `LICENSE`: package metadata author fields and legal attribution.
//! - `examples/tally/dist/`: generated build output (gitignored).
//! - `examples/tally/src-tauri/tauri.conf.json`: product technical identity
//!   (bundle identifier `dev.rustymirror.tally`), explicitly allowlisted.

use std::fs;
use std::path::{Path, PathBuf};

/// Concrete model ids and provider preferences that only an operator should choose.
const MODEL_PATTERNS: &[&str] = &[
    "gpt-5",
    "gpt-6",
    "deepseek",
    "opencode-go/",
    "openrouter/",
    ":exacto",
    "mimo-v",
    "glm-",
    "claude-",
    "haiku",
    "sonnet",
    "opus",
];

/// Names of the people and organisations Rusty Mirror was first built for: never in the shipped tool.
const PERSONAL_PATTERNS: &[&str] = &[
    "Mike",
    "Berardi",
    "Felix",
    "Mouseia",
    "Liberty Design",
    "libertydesign",
    "/Users/",
];

/// Directories to skip entirely.
const SKIP_DIRS: &[&str] = &[".git", "target", "dist", "node_modules"];

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
    if path_str.ends_with("crates/rusty-mirror/tests/neutrality.rs") {
        return true;
    }
    for allow in ALLOWLIST {
        if path_str.ends_with(allow) {
            return true;
        }
    }
    false
}

/// Load private terms without storing them in the repository. Missing files are
/// expected on contributor machines; other errors should not silently disable the scan.
fn load_private_terms(path: &Path) -> Vec<String> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => panic!(
            "Cannot read private terms file {}: {}",
            path.display(),
            error
        ),
    };
    content
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_lowercase)
        .collect()
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Match literally, with a word boundary only at each word-character end.
fn matches_private(line: &str, term: &str) -> bool {
    line.match_indices(term).any(|(start, matched)| {
        let end = start + matched.len();
        let left_ok = !term.chars().next().is_some_and(is_word)
            || line[..start]
                .chars()
                .next_back()
                .map_or(true, |c| !is_word(c));
        let right_ok = !term.chars().next_back().is_some_and(is_word)
            || line[end..].chars().next().map_or(true, |c| !is_word(c));
        left_ok && right_ok
    })
}

fn scan_dir(dir: &Path, private_terms: &[String], hits: &mut Vec<String>) {
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
            scan_dir(&path, private_terms, hits);
        } else if path.is_file() {
            scan_file(&path, private_terms, hits);
        }
    }
}

fn scan_file(path: &Path, private_terms: &[String], hits: &mut Vec<String>) {
    let content = match fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return, // Skip binary or unreadable files.
    };
    // Case-insensitive: a lowercase handle (e.g. a GitHub owner in a URL) must not
    // slip past a capitalized pattern name.
    let lowered = content.to_lowercase();
    let lowered_lines: Vec<&str> = lowered.lines().collect();
    for (i, line) in content.lines().enumerate() {
        let needle = lowered_lines.get(i).copied().unwrap_or("");
        for pattern in MODEL_PATTERNS {
            if needle.contains(&pattern.to_lowercase()) {
                hits.push(format!(
                    "{}:{}: [model] {}",
                    path.display(),
                    i + 1,
                    line.trim()
                ));
            }
        }
        if private_terms
            .iter()
            .any(|term| matches_private(needle, term))
        {
            // Do not expose the operator's private terms in test output.
            hits.push(format!("{}:{}: [private]", path.display(), i + 1));
        }
        // The public repository owner is attribution, not a personal-name leak.
        let personal_line = needle.replace("michael-berardi", "");
        for pattern in PERSONAL_PATTERNS {
            if personal_line.contains(&pattern.to_lowercase()) {
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

    let terms_path = std::env::var_os("OSS_PRIVATE_TERMS")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").expect("HOME is not set"))
                .join(".config/oss-audit/names.txt")
        });
    let private_terms = load_private_terms(&terms_path);
    let mut hits = Vec::new();
    for target in targets {
        if target.is_dir() {
            scan_dir(&target, &private_terms, &mut hits);
        } else if target.is_file() {
            scan_file(&target, &private_terms, &mut hits);
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

#[test]
fn private_terms_loader_and_scanner() {
    let dir = std::env::temp_dir().join(format!(
        "rusty-mirror-neutrality-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir(&dir).unwrap();
    let terms = dir.join("terms.txt");
    let sample = dir.join("sample.txt");
    fs::write(&terms, "# comment\n\nzzqx-private-term\n").unwrap();
    let loaded = load_private_terms(&terms);
    fs::write(&sample, "Contains ZZQX-private-term!\n").unwrap();
    let mut hits = Vec::new();
    scan_file(&sample, &loaded, &mut hits);
    assert_eq!(hits.len(), 1);
    hits.clear();
    fs::write(&sample, "A harmless sample and zzqx-private-terms.\n").unwrap();
    scan_file(&sample, &loaded, &mut hits);
    assert!(hits.is_empty());
    fs::remove_dir_all(dir).unwrap();
}
