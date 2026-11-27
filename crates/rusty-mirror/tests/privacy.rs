//! Privacy regressions: fail if a telemetry/analytics dependency or a
//! third-party endpoint is added, or if the mirror's default CSP opens an
//! outbound destination. Only text files in this checkout are inspected.

use std::{fs, path::Path, path::PathBuf};

/// Packages that must never become a dependency again. Bare `segment` is
/// excluded on purpose: `unicode-segmentation` is an ordinary transitive crate.
const PACKAGES: &[&str] = &[
    "sentry",
    "posthog",
    "mixpanel",
    "amplitude",
    "bugsnag",
    "datadog",
    "hotjar",
    "plausible",
    "umami",
    "matomo",
    "snowplow",
    "countly",
    "google-analytics",
    "googletagmanager",
    "segment-analytics",
    "telemetry",
    "analytics",
];

/// Endpoints the shipped code, docs and example UI must not contact.
const ENDPOINTS: &[&str] = &[
    "sentry.io",
    "posthog.com",
    "mixpanel.com",
    "amplitude.com",
    "bugsnag.com",
    "datadoghq.com",
    "google-analytics.com",
    "googletagmanager.com",
    "segment.io",
    "plausible.io",
    "umami.dev",
    "fonts.googleapis.com",
    "fonts.gstatic.com",
    "cdn.jsdelivr.net",
    "unpkg.com",
    "img.shields.io",
];

const SKIP_DIRS: &[&str] = &[
    ".git",
    "target",
    "dist",
    "gen",
    "node_modules",
    ".cargo-home",
];
const TEXT_EXTS: &[&str] = &[
    "md", "rs", "toml", "lock", "js", "sh", "json", "html", "css",
];
const CODE_EXTS: &[&str] = &["rs", "toml", "lock", "js", "sh", "json", "html", "css"];

fn root() -> PathBuf {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest
        .parent()
        .and_then(Path::parent)
        .expect("crate is two levels below the workspace root")
        .to_path_buf()
}

fn scan_dir(dir: &Path, hits: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() {
            if !SKIP_DIRS.contains(&name) {
                scan_dir(&path, hits);
            }
            continue;
        }
        if path
            .as_os_str()
            .to_string_lossy()
            .ends_with("crates/rusty-mirror/tests/privacy.rs")
        {
            continue; // this test contains the very patterns it looks for
        }
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !TEXT_EXTS.contains(&ext) {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let check_packages = CODE_EXTS.contains(&ext);
        for (i, line) in text.lines().enumerate() {
            let lower = line.to_ascii_lowercase();
            let found = ENDPOINTS
                .iter()
                .filter(|p| lower.contains(**p))
                .map(|p| format!("endpoint {p}"))
                .chain(if check_packages {
                    PACKAGES
                        .iter()
                        .filter(|p| lower.contains(**p))
                        .map(|p| format!("package {p}"))
                        .collect::<Vec<_>>()
                } else {
                    Vec::new()
                });
            for what in found {
                hits.push(format!("{}:{}: {what}", path.display(), i + 1));
            }
        }
    }
}

#[test]
fn no_telemetry_packages_or_third_party_endpoints() {
    let mut hits = Vec::new();
    scan_dir(&root(), &mut hits);
    assert!(hits.is_empty(), "privacy regression:\n{}", hits.join("\n"));
}

#[test]
fn default_csp_only_permits_the_app_origin_and_ipc() {
    let csp = rusty_mirror::DEFAULT_CSP.to_ascii_lowercase();
    assert!(csp.contains("default-src 'self'"));
    assert!(csp.contains("connect-src 'self' ipc: http://ipc.localhost"));
    assert_eq!(
        csp.matches("http://").count(),
        1,
        "unexpected remote URL in CSP"
    );
    assert!(!csp.contains("https://"));
}

#[test]
fn readme_privacy_is_short_and_privacy_md_has_the_details() {
    let readme = fs::read_to_string(root().join("README.md")).unwrap();
    let section = readme
        .split("\n## Privacy\n")
        .nth(1)
        .expect("README has a Privacy section");
    let section = section.split("\n## ").next().unwrap();
    let lines = section.lines().filter(|l| !l.trim().is_empty()).count();
    assert!(
        lines <= 6,
        "README Privacy has {lines} lines; details belong in PRIVACY.md"
    );
    assert!(section.contains("PRIVACY.md"));
    assert!(section.contains("daily update check"));
    let privacy = fs::read_to_string(root().join("PRIVACY.md")).unwrap();
    for detail in [
        "control.sock",
        "hot-assets",
        "missing parent\n  directories are created 0700",
        "TALLY_DATA",
        "localStorage",
        "update.json",
        "rusty-mirror-update-check",
        "RUSTY_MIRROR_NO_UPDATE_CHECK=1",
        "SHA256SUMS",
        "default CSP",
    ] {
        assert!(privacy.contains(detail), "PRIVACY.md lacks {detail:?}");
    }
    for text in [&readme, &privacy] {
        let lower = text.to_ascii_lowercase();
        assert!(!lower.contains("no update check"), "stale update wording");
    }
}
