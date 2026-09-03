mod common;
mod support;

#[path = "cases/http_track_file.rs"]
mod http_track_file;
#[path = "cases/plugin_scope.rs"]
mod plugin_scope;
#[path = "cases/rest_track_report.rs"]
mod rest_track_report;
#[path = "cases/track_create_sync_daemon.rs"]
mod track_create_sync_daemon;
#[path = "cases/track_create_with_theme.rs"]
mod track_create_with_theme;
#[path = "cases/track_cwd_terminal_at.rs"]
mod track_cwd_terminal_at;
#[path = "cases/track_delete_forge_fence.rs"]
mod track_delete_forge_fence;
#[path = "cases/track_fsm_golden.rs"]
mod track_fsm_golden;
#[path = "cases/track_pin.rs"]
mod track_pin;
#[path = "cases/track_report_fork.rs"]
mod track_report_fork;
#[path = "cases/track_report_write_origin.rs"]
mod track_report_write_origin;
#[path = "cases/track_template_overlay.rs"]
mod track_template_overlay;
#[path = "cases/track_template_tracks.rs"]
mod track_template_tracks;
#[path = "cases/track_templates_read.rs"]
mod track_templates_read;
#[path = "cases/track_vcs.rs"]
mod track_vcs;
