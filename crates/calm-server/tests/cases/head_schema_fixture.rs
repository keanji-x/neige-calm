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
    "0090_track_vcs_commit_prefix_index.sql",
    "0091_terminal_output_evidence.sql",
    "0092_track_create_message_less_binding.sql",
    "0093_operations_keyed_rows_are_permanent.sql",
    "0094_runtime_id_to_worker_session_id.sql",
    "0095_worker_sessions_queue_harvested.sql",
    "0096_area_create_idempotency.sql",
    "0097_task_attempt_allocations.sql",
    "0098_harness_queue_changed_event_version.sql",
    "0099_isolated_parked_operation_receipts.sql",
    "0100_task_execution_settled_event_version.sql",
    "0101_planner_recovery_bindings.sql",
    "0102_task_file_delivery.sql",
    "0103_candidate_verification.sql",
    "0104_candidate_review.sql",
    "0105_planner_dispatch_receipts.sql",
    "0106_candidate_repair.sql",
    "0107_report_series.sql",
    "0108_report_sources.sql",
    "0109_track_claude_permissions_policy.sql",
    "0110_database_identity_and_transcript_index.sql",
    "0111_workspace_lease_base.sql",
    "0112_git_delivery_settled_event_version.sql",
    "0113_task_git_deliveries.sql",
    "0114_task_git_delivery_abandonments.sql",
    "0115_workspace_lease_upstream_base.sql",
    "0116_task_replacements.sql",
    "0117_planner_provider_card_key.sql",
    "0118_worktree_reclaim_indexes.sql",
    "0119_report_tags.sql",
    "0120_track_worktree.sql",
    "0121_activity_dismissals.sql",
    "0122_task_git_delivery_outcome.sql",
    "0123_track_closed_at.sql",
    "0124_drop_track_claude_permissions_policy.sql",
    "0125_drop_file_delivery.sql",
    "0126_drop_task_git_delivery_abandonments.sql",
    "0127_drop_planner_recovery_bindings.sql",
    "0128_drop_planner_dispatch_receipts.sql",
    "0129_plugin_data_changed_event_version.sql",
    "0130_read_only_tasks.sql",
    "0131_read_only_task_commits.sql",
    "0132_track_wake_requested_event_version.sql",
    "0133_harness_transcript_rewound_event_version.sql",
    "0134_neige_tool_names.sql",
    "0135_worker_report_recipe_names.sql",
    "0136_managed_daily_tracks.sql",
    "0137_neige_dev_publish.sql",
    "0138_task_start.sql",
    "0139_planner_input_idempotency.sql",
    "0140_dev_template.sql",
    "0141_tool_name_separator.sql",
    "0142_tool_verbs.sql",
    "0143_terminal_verbs.sql",
    "0144_crud_verbs.sql",
    "0145_task_git_delivery_commit_message.sql",
    "0146_mails.sql",
    "0147_track_creator_provenance.sql",
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
