//! A source scan: SDK clients are built in test code only under
//! `tests/net/`, where [`crate::hermetic`] pins the environment. Elsewhere
//! (the lib's unit tests, the suite) a regression could make the SDK probe
//! `geotest.lbkrs.com` or reach the OAuth host. Production code that builds
//! one is named here explicitly.

use std::path::{Path, PathBuf};

/// What builds an SDK client or a source.
const CONSTRUCTIONS: [&str; 4] = [
    "build_oauth(",
    "OAuthBuilder",
    "LongbridgeSource::new(",
    "OAuth::from_token(",
];

/// Production files allowed to build one, relative to the package.
const PRODUCTION: [&str; 2] = ["src/auth.rs", "src/source.rs"];

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn sdk_clients_are_built_only_under_the_hermetic_harness() {
    let package = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    rust_files(&package.join("src"), &mut files);
    rust_files(&package.join("tests"), &mut files);
    assert!(files.len() > 20, "the scan found the sources");

    let mut escaped = Vec::new();
    for path in &files {
        let relative = path.strip_prefix(package).unwrap();
        let allowed = relative.starts_with("tests/net")
            || PRODUCTION.iter().any(|p| relative == Path::new(p));
        if allowed {
            continue;
        }
        let text = std::fs::read_to_string(path).unwrap();
        for (n, line) in text.lines().enumerate() {
            for construction in CONSTRUCTIONS {
                if line.contains(construction) {
                    escaped.push(format!("{}:{}: {construction}", relative.display(), n + 1));
                }
            }
        }
    }
    assert!(escaped.is_empty(), "outside tests/net: {escaped:#?}");
}
