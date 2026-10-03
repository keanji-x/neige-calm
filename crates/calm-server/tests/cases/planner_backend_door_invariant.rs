//! Source scan (#1981 S3): `PlannerBackend` is the only provider door of the Planner harness.
//! Its arms are matched only inside `harness/backend.rs`, and it has no `codex()` escape hatch
//! that hands the Codex daemon to generic code. A test module may still construct a backend.

use std::path::{Path, PathBuf};

const BACKEND: &str = "src/harness/backend.rs";
const ARMS: [&str; 2] = ["PlannerBackend::Codex", "PlannerBackend::Claude"];

#[test]
fn planner_backend_is_the_only_provider_door() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let backend = std::fs::read_to_string(manifest_dir.join(BACKEND)).expect("read backend.rs");
    assert!(
        backend.contains("pub enum PlannerBackend"),
        "{BACKEND} no longer defines PlannerBackend; move this scan with it"
    );
    assert!(
        !backend.contains("fn codex("),
        "{BACKEND} defines `fn codex(` again; generic code must not reach the Codex daemon"
    );
    let mut scanned = 0;
    for path in non_test_rust_files(&manifest_dir.join("src")) {
        let rel = path
            .strip_prefix(&manifest_dir)
            .expect("path under manifest dir")
            .to_string_lossy()
            .replace('\\', "/");
        if rel == BACKEND {
            continue;
        }
        scanned += 1;
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        for arm in ARMS {
            assert!(
                !src.contains(arm),
                "{rel} names `{arm}`; add a neutral PlannerBackend method in {BACKEND} instead"
            );
        }
    }
    assert!(scanned > 100, "the scan found only {scanned} source files");
}

/// Every `.rs` under `dir`, except `tests.rs`, `*_tests.rs` and anything under a `tests` dir.
fn non_test_rust_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    visit(dir, &mut out);
    out
}

fn visit(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}")) {
        let path = entry.expect("read_dir entry").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("utf-8 file name");
        if path.is_dir() {
            if name != "tests" {
                visit(&path, out);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" && !name.ends_with("_tests.rs") {
            out.push(path);
        }
    }
}
