//! Snapshot guard: planner harness turn issuance must stay behind the private owner-bound `IssueTurnHandle`.
//! If this test starts failing, audit the new caller: non-reconciliation issuance bypasses the queue/guard invariant.

use std::path::{Path, PathBuf};
use syn::visit::{self, Visit};

#[test]
fn harness_turn_start_is_gated() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let issuance_path = "src/harness/run_loop/issuance.rs";
    let issuance =
        std::fs::read_to_string(manifest_dir.join(issuance_path)).expect("read ordinary issuance");
    assert!(issuance.contains("IssueTurnHandle::from_reconciliation(inner)"));
    OwnerBoundStarts.visit_file(&syn::parse_file(&issuance).expect("parse ordinary issuance"));
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
        managed.matches(".start_turn(").count(),
        turn_start_fn.matches(".start_turn(").count(),
        "ACP starts stay inside the private owner-bound start"
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
            // Ordinary issuance uses the owner-bound handle, never a native/session start.
            (!src.contains(".turn_start(") || rel == issuance_path)
                && !src.contains(".turn_start_control(")
                && !src.contains(".start_turn("),
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

/// The ordinary caller may use the handle, but must not open a second native/session start path.
struct OwnerBoundStarts;
impl<'ast> Visit<'ast> for OwnerBoundStarts {
    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if call.method == "turn_start" {
            assert!(
                owner_bound_receiver(&call.receiver),
                "ordinary start must use its owner-bound handle"
            );
        }
        visit::visit_expr_method_call(self, call);
    }
}

fn owner_bound_receiver(receiver: &syn::Expr) -> bool {
    let syn::Expr::Await(awaited) = receiver else {
        return false;
    };
    let syn::Expr::Call(call) = &*awaited.base else {
        return false;
    };
    let syn::Expr::Path(binding) = &*call.func else {
        return false;
    };
    binding
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .eq(["IssueTurnHandle", "from_reconciliation"])
        && call.args.len() == 1
        && matches!(call.args.first(), Some(syn::Expr::Path(owner)) if owner.path.is_ident("inner"))
}

#[test]
fn ordinary_start_requires_the_owner_bound_receiver() {
    for (expression, admitted) in [
        (
            "IssueTurnHandle::from_reconciliation(inner).await.turn_start(request, claim, rewind)",
            true,
        ),
        ("daemon.turn_start(request)", false),
        ("session.turn_start(request)", false),
        (
            "IssueTurnHandle::from_reconciliation(other).await.turn_start(request, claim, rewind)",
            false,
        ),
        (
            "AnotherHandle::from_reconciliation(inner).await.turn_start(request, claim, rewind)",
            false,
        ),
    ] {
        let call: syn::ExprMethodCall = syn::parse_str(expression).unwrap();
        assert_eq!(
            owner_bound_receiver(&call.receiver),
            admitted,
            "{expression}"
        );
    }
}
