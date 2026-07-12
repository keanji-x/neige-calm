mod common;
mod support;

#[path = "cases/actor.rs"]
mod actor;
#[path = "cases/admin_maintenance.rs"]
mod admin_maintenance;
#[path = "cases/auth.rs"]
mod auth;
#[path = "cases/card_cascade_semantics.rs"]
mod card_cascade_semantics;
#[path = "cases/cards_deletable.rs"]
mod cards_deletable;
#[path = "cases/claude_fsm_overlay.rs"]
mod claude_fsm_overlay;
#[path = "cases/claude_ingest.rs"]
mod claude_ingest;
#[path = "cases/cove_folders.rs"]
mod cove_folders;
#[path = "cases/cove_system_endpoint.rs"]
mod cove_system_endpoint;
#[path = "cases/dispatcher_real_auth_path.rs"]
mod dispatcher_real_auth_path;
#[path = "cases/dispatcher_role_scope.rs"]
mod dispatcher_role_scope;
#[path = "cases/frozen_gate_vectors.rs"]
mod frozen_gate_vectors;
#[path = "cases/frozen_gate_vectors_transport.rs"]
mod frozen_gate_vectors_transport;
#[path = "cases/in_process_renderer_e2e.rs"]
mod in_process_renderer_e2e;
#[path = "cases/neige_cli_task_report.rs"]
mod neige_cli_task_report;
#[path = "cases/openapi.rs"]
mod openapi;
#[path = "cases/payload_validation.rs"]
mod payload_validation;
#[path = "cases/repo.rs"]
mod repo;
#[path = "cases/review_ratify.rs"]
mod review_ratify;
#[path = "cases/role_enforcement.rs"]
mod role_enforcement;
#[path = "cases/settings.rs"]
mod settings;
#[path = "cases/threads_api.rs"]
mod threads_api;
#[path = "cases/threads_resolve_claude.rs"]
mod threads_resolve_claude;
#[path = "cases/today_launchpad.rs"]
mod today_launchpad;
#[path = "cases/version.rs"]
mod version;
