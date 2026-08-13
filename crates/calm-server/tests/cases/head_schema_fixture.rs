//! Keeps the post-0067 migration filename inventory synchronized with disk.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

const POST_0067_MIGRATION_NAMES: &[&str] = &[
    "0068_projection_policy_columns.sql",
    "0069_clear_pending_context_stale.sql",
    "0070_task_context_withdrawal_and_verify.sql",
    "0071_sub_wave_tree.sql",
    "0072_wave_tree_task_budget.sql",
    "0073_drop_task_origin.sql",
];

#[test]
fn head_schema_fixture_lists_every_migration_from_0068_through_head() {
    let migrations = Path::new(env!("CARGO_MANIFEST_DIR")).join("../calm-truth/migrations");
    let on_disk = fs::read_dir(migrations)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.as_str() >= "0068_")
        .collect::<BTreeSet<_>>();
    let fixture = POST_0067_MIGRATION_NAMES
        .iter()
        .map(|name| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(fixture, on_disk, "head-schema migration fixture drifted");
}
