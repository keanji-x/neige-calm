//! Snapshot guard: planner harness turn issuance must stay behind the private owner-bound `IssueTurnHandle`.
//! If this test starts failing, audit the new caller: non-reconciliation issuance bypasses the queue/guard invariant.

use std::path::{Path, PathBuf};

#[test]
fn harness_turn_start_is_gated() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let run_loop_path = manifest_dir.join("src/harness/run_loop.rs");
    let run_loop = std::fs::read_to_string(&run_loop_path).expect("read run_loop.rs");

    assert!(run_loop.contains("IssueTurnHandle::from_reconciliation(inner)"));
    let managed = std::fs::read_to_string(manifest_dir.join("src/harness/backend/managed.rs"))
        .expect("read private managed entry");
    assert!(managed.contains("pub(in crate::harness) struct IssueTurnHandle<'a>"));
    assert!(managed.contains("owner: &'a Inner"));
    assert!(!managed.contains("pub thread") && !managed.contains("pub owner"));
    let turn_start_fn = managed
        .split("pub(in crate::harness) async fn turn_start(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .expect("owner-bound start present");
    assert!(
        !turn_start_fn
            .split(" -> ")
            .next()
            .unwrap()
            .contains("thread_id")
    );
    assert_eq!(
        managed.matches(".turn_start_control(").count(),
        turn_start_fn.matches(".turn_start_control(").count(),
        "native sends stay inside the private owner-bound start"
    );

    let allowed = [
        "src/dispatcher/mod.rs",
        "src/harness/backend/managed.rs",
        "src/operation/codex_adapter/mod.rs",
        "src/shared_codex_appserver.rs",
        "src/claude_planner/session.rs",
        "src/acp_planner/session.rs",
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
        if is_test_only_source(&src) {
            continue;
        }
        assert!(
            !src.contains(".turn_start(") && !src.contains(".turn_start_control("),
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

/// Only a file-level unconditional `cfg(test)` proves that no caller in the file can
/// enter a production build. File names, comments and attributes on individual items do not.
fn is_test_only_source(source: &str) -> bool {
    syn::parse_file(source)
        .expect("parse source for file-level test configuration")
        .attrs
        .iter()
        .any(|attr| {
            attr.path().is_ident("cfg")
                && attr
                    .parse_args::<syn::Path>()
                    .is_ok_and(|path| path.is_ident("test"))
        })
}

#[test]
fn turn_start_scan_excludes_only_rust_enforced_test_files() {
    assert!(is_test_only_source("#![cfg(test)]\nfn fixture() {}"));
    assert!(is_test_only_source(
        "//! test fixtures\n#![cfg(test)]\nfn fixture() {}"
    ));
    for source in [
        "// #![cfg(test)]\nfn production() {}",
        "const MARKER: &str = \"#![cfg(test)]\";",
        "#[cfg(test)] mod tests {} fn production() {}",
        "#![cfg(any(test, unix))]\nfn production() {}",
        "#![cfg_attr(test, allow(dead_code))]\nfn production() {}",
        "fn production() {}",
    ] {
        assert!(!is_test_only_source(source), "must scan: {source}");
    }
}
