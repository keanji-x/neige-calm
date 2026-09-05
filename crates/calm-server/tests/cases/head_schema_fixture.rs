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
    "0074_one_chat_wave_per_cove.sql",
    "0075_drop_cove_folder_repo_identity.sql",
    "0076_waves_plugin_scope.sql",
    "0077_wave_workspace.sql",
    "0078_cards_role_assistant.sql",
    "0079_waves_rename_workflow_id_to_template_id.sql",
    "0080_cove_to_area.sql",
    "0081_wave_to_track.sql",
    "0082_track_recipes.sql",
    "0083_spec_to_planner.sql",
    "0084_harness_input_segments.sql",
    "0085_track_recipe_provenance.sql",
    "0086_rename_worker_flow_items_runtime_id.sql",
    "0087_area_track_defaults.sql",
    "0088_track_create_idempotency.sql",
    "0089_track_create_request_fingerprint.sql",
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
