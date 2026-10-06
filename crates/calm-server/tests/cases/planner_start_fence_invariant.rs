//! #2252 — every route-level `planner-harness-start` goes through `CardStartFence::start`, which
//! holds the card's `planner_recovery_locks` guard through the submit and its wait. A start
//! submitted anywhere else can interleave with a send's recovery or another start of the card.
//! If this test fails, route the new submit through the fence; a new read-only lookup of the
//! kind is added below with its reason.

use std::path::{Path, PathBuf};

const KIND_LITERAL: &str = "\"planner-harness-start\"";
/// Every exported name for the kind: the server's const and calm-truth's copy, which a route could
/// otherwise submit with past this scan.
const KIND_CONSTS: &[&str] = &["PLANNER_HARNESS_START", "PLANNER_START_OPERATION_KIND"];

/// `(file, literal mentions, const mentions, why)`. Comment lines are not counted; imports are,
/// so an alias cannot carry the kind past this list.
const ALLOWED: &[(&str, usize, usize, &str)] = &[
    (
        "src/routes/conversations_shared.rs",
        1,
        2,
        "the const's definition, and `retryable_operation_key`'s read-only lookup",
    ),
    ("src/operation/mod.rs", 1, 0, "the operation registry"),
    (
        "src/operation/planner_harness_start_adapter.rs",
        1,
        1,
        "the adapter's `kind()`, and one unit-test fixture operation row",
    ),
    (
        "src/operation/planner_start_read_contract_tests.rs",
        0,
        6,
        "the read-contract test, which pins `kind()` and calm-truth's name to the const",
    ),
    (
        "src/routes/tracks/create.rs",
        0,
        2,
        "the keyed create's read-only `find_by_kind_and_idempotency` lookup",
    ),
    (
        "src/routes/today.rs",
        0,
        2,
        "the launchpad's read-only \"a start succeeded at this path\" lookup",
    ),
    (
        "src/routes/planner_start_fence.rs",
        0,
        2,
        "the fence: the only route-level submit",
    ),
    (
        "src/scheduler/mod.rs",
        0,
        1,
        "the one named exception: the child-track bootstrap. The scheduler holds only a `Weak` \
         operation runtime and is built before `RouteState`'s lock maps, so it does not yet hold \
         the card's lock; sharing the lock map from boot would remove this entry",
    ),
];

/// Each allowed mention, as it must read with all whitespace removed.
const SHAPES: &[(&str, &str)] = &[
    (
        "src/routes/conversations_shared.rs",
        "find_by_kind_and_idempotency(PLANNER_HARNESS_START,",
    ),
    (
        "src/operation/planner_harness_start_adapter.rs",
        "fnkind(&self)->&'staticstr{\"planner-harness-start\"}",
    ),
    (
        "src/routes/tracks/create.rs",
        "find_by_kind_and_idempotency(PLANNER_HARNESS_START,",
    ),
    ("src/routes/today.rs", ".bind(PLANNER_HARNESS_START)"),
    (
        "src/routes/planner_start_fence.rs",
        ".submit(PLANNER_HARNESS_START,",
    ),
    (
        "src/scheduler/mod.rs",
        ".submit(crate::routes::conversations_shared::PLANNER_HARNESS_START,",
    ),
];

#[test]
fn planner_harness_start_is_submitted_only_through_the_card_start_fence() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut unexpected = Vec::new();
    for path in rust_files(&manifest_dir.join("src")) {
        let rel = path
            .strip_prefix(&manifest_dir)
            .expect("path under manifest dir")
            .to_string_lossy()
            .replace('\\', "/");
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {rel}: {e}"));
        let code: Vec<&str> = src
            .lines()
            .filter(|line| !line.trim_start().starts_with("//"))
            .collect();
        let literal = code
            .iter()
            .map(|line| line.matches(KIND_LITERAL).count())
            .sum::<usize>();
        let constant = code
            .iter()
            .map(|line| {
                KIND_CONSTS
                    .iter()
                    .map(|name| line.matches(name).count())
                    .sum::<usize>()
            })
            .sum::<usize>();
        let expected = ALLOWED
            .iter()
            .find(|(file, ..)| *file == rel)
            .map_or((0, 0), |(_, literal, constant, _)| (*literal, *constant));
        if (literal, constant) != expected {
            unexpected.push(format!(
                "{rel}: {literal} literal and {constant} const mentions of the \
                 `planner-harness-start` kind, expected {expected:?}"
            ));
        }
    }
    assert!(
        unexpected.is_empty(),
        "`planner-harness-start` is submitted only by `CardStartFence::start` \
         (src/routes/planner_start_fence.rs); a new mention needs the fence or a reviewed entry \
         in ALLOWED:\n{}",
        unexpected.join("\n")
    );

    for (file, shape) in SHAPES {
        let src = std::fs::read_to_string(manifest_dir.join(file))
            .unwrap_or_else(|e| panic!("read {file}: {e}"));
        let flat: String = src.chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(
            flat.matches(shape).count(),
            1,
            "{file} should mention the kind exactly once as `{shape}`"
        );
    }
    let fence = std::fs::read_to_string(manifest_dir.join("src/routes/planner_start_fence.rs"))
        .expect("read the fence");
    assert_eq!(
        fence.matches(".submit(").count(),
        1,
        "the fence submits from `CardStartFence::start` only"
    );
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
