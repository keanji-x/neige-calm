//! Keeps the hand-applied HEAD-schema fixture synchronized with migrations.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

const MIGRATION_0068: &str =
    include_str!("../../../calm-truth/migrations/0068_projection_policy_columns.sql");
const MIGRATION_0069: &str =
    include_str!("../../../calm-truth/migrations/0069_clear_pending_context_stale.sql");
const MIGRATION_0070: &str =
    include_str!("../../../calm-truth/migrations/0070_task_context_withdrawal_and_verify.sql");
const MIGRATION_0071: &str = include_str!("../../../calm-truth/migrations/0071_sub_wave_tree.sql");
const MIGRATION_0072: &str =
    include_str!("../../../calm-truth/migrations/0072_wave_tree_task_budget.sql");
const MIGRATION_0073: &str =
    include_str!("../../../calm-truth/migrations/0073_drop_task_origin.sql");

const HEAD_SCHEMA_FIXTURE_MIGRATIONS: &[(&str, &str)] = &[
    ("0068_projection_policy_columns.sql", MIGRATION_0068),
    ("0069_clear_pending_context_stale.sql", MIGRATION_0069),
    (
        "0070_task_context_withdrawal_and_verify.sql",
        MIGRATION_0070,
    ),
    ("0071_sub_wave_tree.sql", MIGRATION_0071),
    ("0072_wave_tree_task_budget.sql", MIGRATION_0072),
    ("0073_drop_task_origin.sql", MIGRATION_0073),
];

#[test]
fn head_schema_fixture_lists_every_migration_from_0068_through_head() {
    let migrations = Path::new(env!("CARGO_MANIFEST_DIR")).join("../calm-truth/migrations");
    let on_disk = fs::read_dir(migrations)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.as_str() >= "0068_")
        .collect::<BTreeSet<_>>();
    let fixture = HEAD_SCHEMA_FIXTURE_MIGRATIONS
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect::<BTreeSet<_>>();
    assert_eq!(fixture, on_disk, "head-schema migration fixture drifted");
}
