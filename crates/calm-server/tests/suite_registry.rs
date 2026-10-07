//! Prevent autotests=false from silently dropping a newly added root test file.
#[test]
fn every_root_test_file_is_in_this_suite() {
    let manifest: toml::Value = toml::from_str(include_str!("../Cargo.toml")).unwrap();
    let mut declared: Vec<String> = manifest["test"]
        .as_array()
        .unwrap()
        .iter()
        .map(|target| {
            target["path"]
                .as_str()
                .unwrap()
                .strip_prefix("tests/")
                .unwrap()
                .to_owned()
        })
        .collect();
    for source in [
        include_str!("api_suite.rs"),
        include_str!("runtime_suite.rs"),
    ] {
        declared.extend(source.lines().filter_map(|line| {
            line.trim()
                .strip_prefix("#[path = \"")?
                .strip_suffix("\"]")
                .map(str::to_owned)
        }));
    }
    let mut actual: Vec<_> = std::fs::read_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/tests"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| name.ends_with(".rs"))
        .collect();
    declared.sort_unstable();
    actual.sort_unstable();
    assert_eq!(
        declared, actual,
        "a root test file is missing or registered more than once"
    );
}
