fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().plugin(
        "bundled-frontend",
        tauri_build::InlinedPlugin::new().commands(&[
            "bind_server",
            "enroll_from_scan",
            "cancel_enrollment",
            "reset_enrollment",
            "connection_settings",
            "save_connection",
            "select_saved_tailnet",
            "confirm_legacy_tailnet",
            "attempt_connection",
            "cancel_connection",
        ]),
    ))
    .expect("failed to build the mobile application permissions");
}
