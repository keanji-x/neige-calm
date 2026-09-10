#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .setup(|app| {
            #[cfg(mobile)]
            app.handle().plugin(tauri_plugin_barcode_scanner::init())?;
            #[cfg(target_os = "android")]
            app.handle().plugin(
                tauri::plugin::Builder::<tauri::Wry>::new("bundled-frontend")
                    .setup(|_, api| {
                        api.register_android_plugin("io.neigecalm.next", "BundledFrontendPlugin")?;
                        Ok(())
                    })
                    .build(),
            )?;
            #[cfg(not(mobile))]
            let _ = app;
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
