#[path = "append_seam_trybuild.rs"]
mod append_seam_trybuild;
#[path = "bounded_track_tree_sql.rs"]
mod bounded_track_tree_sql;
#[path = "events_since_bound.rs"]
mod events_since_bound;
#[path = "track_vcs_prune.rs"]
mod track_vcs_prune;
#[path = "track_write_point_registry.rs"]
mod track_write_point_registry;
#[path = "worker_session_matrix_alignment.rs"]
mod worker_session_matrix_alignment;
#[path = "worker_sessions_nonterminal.rs"]
mod worker_sessions_nonterminal;

#[test]
fn every_root_test_file_is_in_this_suite() {
    assert_suite_complete(include_str!("integration_suite.rs"));
}

fn assert_suite_complete(source: &str) {
    let mut declared: Vec<_> = source
        .lines()
        .filter_map(|line| line.trim().strip_prefix("#[path = \"")?.strip_suffix("\"]"))
        .map(str::to_owned)
        .collect();
    let mut actual: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".rs") && name != "integration_suite.rs")
        .collect();
    declared.sort_unstable();
    actual.sort_unstable();
    assert_eq!(
        declared, actual,
        "a root test file is missing from the suite"
    );
}
