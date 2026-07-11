//! Snapshot guard: spec harness turn issuance must stay behind
//! `run_loop::IssueTurnHandle`.
//!
//! If this test starts failing, audit the new caller. Non-reconciliation
//! harness turn issuance bypasses the queue/guard invariant (#550 F3).

use std::path::{Path, PathBuf};

#[test]
fn harness_turn_start_is_gated() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let run_loop_path = manifest_dir.join("src/harness/run_loop.rs");
    let run_loop = std::fs::read_to_string(&run_loop_path).expect("read run_loop.rs");

    let run_loop_turn_start_count = run_loop.matches(".turn_start(").count();
    assert_eq!(
        run_loop_turn_start_count, 1,
        "run_loop.rs should call .turn_start only inside IssueTurnHandle::issue"
    );
    assert!(
        run_loop.contains("IssueTurnHandle::from_reconciliation(inner)"),
        "maybe_issue_turn should construct IssueTurnHandle from reconciliation"
    );

    let allowed = [
        "src/dispatcher/mod.rs",
        "src/harness/run_loop.rs",
        "src/operation/codex_adapter/mod.rs",
        "src/shared_codex_appserver.rs",
    ];
    for path in rust_files(&manifest_dir.join("src")) {
        let rel = path
            .strip_prefix(&manifest_dir)
            .expect("path under manifest dir")
            .to_string_lossy()
            .replace('\\', "/");
        if allowed.contains(&rel.as_str()) {
            continue;
        }
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        assert!(
            !src.contains(".turn_start("),
            "unexpected turn_start callsite in {rel}"
        );
    }
}

fn rust_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    visit_rust_files(root, &mut out);
    out
}

fn visit_rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}")) {
        let entry = entry.expect("read_dir entry");
        let path = entry.path();
        if path.is_dir() {
            visit_rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}
