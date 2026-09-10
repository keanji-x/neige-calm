fn main() {
    tauri_build::try_build(tauri_build::Attributes::new().plugin(
        "bundled-frontend",
        tauri_build::InlinedPlugin::new().commands(&[
            "bind_server",
            "login_tailscale",
            "connection_settings",
            "save_connection",
            "attempt_connection",
        ]),
    ))
    .expect("failed to build the mobile application permissions");
}
