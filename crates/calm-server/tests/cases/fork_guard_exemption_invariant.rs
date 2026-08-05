use std::path::PathBuf;

use crate::deferred_write_tx_invariant::{normalize_source, production_lines, rust_files};

#[test]
fn task_guard_rule_one_exemption_has_exactly_one_production_call_site() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir.join("src");
    assert!(root.is_dir(), "calm-server source root vanished: {root:?}");

    let mut scanned = 0usize;
    let mut saw_guard_module = false;
    let mut saw_wave_route = false;
    let mut saw_regular_guard_call = false;
    let mut fork_calls = Vec::new();
    let mut exemption_variants = Vec::new();

    for path in rust_files(&root) {
        scanned += 1;
        let rel = path
            .strip_prefix(&manifest_dir)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        saw_guard_module |= rel == "src/wave_report_task_guard.rs";
        saw_wave_route |= rel == "src/routes/waves.rs";
        let source =
            std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {rel}: {error}"));
        let normalized = normalize_source(&source);
        for (line_number, line) in production_lines(&normalized) {
            let trimmed = line.trim();
            if trimmed.contains("guard_fork_task_declarations(")
                && !trimmed.starts_with("pub(crate) fn guard_fork_task_declarations")
            {
                fork_calls.push(format!("{rel}:{line_number}"));
            }
            if rel == "src/wave_report.rs"
                && trimmed.contains("guard_task_declarations(")
                && !trimmed.starts_with("pub(crate) fn guard_task_declarations")
            {
                saw_regular_guard_call = true;
            }
            let mut rest = line;
            while let Some(index) = rest.find("TaskGuardRule1Exemption::") {
                let suffix = &rest[index + "TaskGuardRule1Exemption::".len()..];
                let variant: String = suffix
                    .chars()
                    .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
                    .collect();
                exemption_variants.push(format!("{variant}@{rel}:{line_number}"));
                rest = &suffix[variant.len()..];
            }
        }
    }

    assert!(
        scanned > 100,
        "scan looks vacuous: only {scanned} Rust files"
    );
    assert!(saw_guard_module, "guard module was outside the scan");
    assert!(saw_wave_route, "wave route was outside the scan");
    assert!(
        saw_regular_guard_call,
        "ordinary guard call in wave_report.rs disappeared from the production scan"
    );
    assert_eq!(
        fork_calls.len(),
        1,
        "fork rule-1 exemption must have exactly one production caller: {fork_calls:?}"
    );
    assert!(
        fork_calls[0].starts_with("src/routes/waves.rs:"),
        "the sole fork exemption caller must be the wave-create route: {fork_calls:?}"
    );
    assert_eq!(
        exemption_variants.len(),
        1,
        "qualified exemption variants must contain only the one Fork use: {exemption_variants:?}"
    );
    assert!(
        exemption_variants[0].starts_with("Fork@src/wave_report_task_guard.rs:"),
        "unexpected exemption variant use: {:?}",
        exemption_variants
    );
}
