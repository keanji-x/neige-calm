#[path = "e2e_bridge_enrichment.rs"]
mod e2e_bridge_enrichment;
#[path = "post_retry.rs"]
mod post_retry;
#[path = "session_id_resolution.rs"]
mod session_id_resolution;
#[path = "stop_hook.rs"]
mod stop_hook;

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
