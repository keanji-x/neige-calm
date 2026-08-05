use std::collections::BTreeSet;
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
    let mut enum_variants = None;

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
        if rel == "src/wave_report_task_guard.rs" {
            let declaration = "enum TaskGuardRule1Exemption";
            let enum_start = normalized
                .find(declaration)
                .expect("TaskGuardRule1Exemption enum definition vanished");
            let body_start = normalized[enum_start..]
                .find('{')
                .map(|offset| enum_start + offset + 1)
                .expect("TaskGuardRule1Exemption enum body vanished");
            let body_end = normalized[body_start..]
                .find('}')
                .map(|offset| body_start + offset)
                .expect("TaskGuardRule1Exemption enum body is unclosed");
            let members = normalized[body_start..body_end]
                .split(',')
                .filter_map(|member| {
                    member
                        .split_whitespace()
                        .find(|token| token.chars().next().is_some_and(char::is_uppercase))
                        .map(|token| {
                            token.trim_matches(|character: char| {
                                !character.is_alphanumeric() && character != '_'
                            })
                        })
                        .filter(|token| !token.is_empty())
                        .map(str::to_string)
                })
                .collect::<BTreeSet<_>>();
            enum_variants = Some(members);
        }
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
            let prefixes: &[&str] = if rel == "src/wave_report_task_guard.rs" {
                &["TaskGuardRule1Exemption::", "Self::"]
            } else {
                &["TaskGuardRule1Exemption::"]
            };
            while let Some((index, prefix)) = prefixes
                .iter()
                .copied()
                .filter_map(|prefix| rest.find(prefix).map(|index| (index, prefix)))
                .min_by_key(|(index, _)| *index)
            {
                let suffix = &rest[index + prefix.len()..];
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
    assert_eq!(
        enum_variants,
        Some(BTreeSet::from(["Fork".to_string(), "None".to_string()])),
        "TaskGuardRule1Exemption definition must contain exactly None and Fork"
    );
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
        2,
        "exemption variant uses must contain only Self::Fork and the one qualified Fork call: {exemption_variants:?}"
    );
    assert!(
        exemption_variants
            .iter()
            .all(|variant| { variant.starts_with("Fork@src/wave_report_task_guard.rs:") }),
        "unexpected exemption variant use: {exemption_variants:?}"
    );
}

#[test]
fn fork_report_cache_write_precedes_task_projection() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let route = manifest_dir.join("src/routes/waves.rs");
    let source = std::fs::read_to_string(&route)
        .unwrap_or_else(|error| panic!("read {}: {error}", route.display()));
    let start = source
        .find("if let Some((payload, mut doc, declarations, diagnostics)) = fork_snapshot")
        .expect("fork snapshot write branch vanished");
    let end = source[start..]
        .find("let wave_scope = EventScope::Wave")
        .map(|offset| start + offset)
        .expect("fork snapshot write branch boundary vanished");
    let branch = &source[start..end];
    let cache_write = branch
        .find("card_update_with_crdt_tx(")
        .expect("fork report cache/CRDT write vanished");
    let projection = branch
        .find("project_tasks_tx(")
        .expect("fork task projection vanished");
    assert!(
        cache_write < projection,
        "fork task projection ran before its reference-existence cache write"
    );
}
