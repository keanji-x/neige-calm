use super::*;

#[test]
fn config_plugin_dirs_reach_child_argv() {
    let path =
        std::env::temp_dir().join(format!("neige-app-plugin-dirs-{}.toml", std::process::id()));
    fs::write(
        &path,
        r#"
[child]
plugins_dir = "~/neige second/plugins"
plugins_data_dir = "~/neige second/plugin-data"
extra_args = ["--plugins-disabled", "example"]
"#,
    )
    .expect("write config");
    let result = AppConfig::load(Some(&path));
    fs::remove_file(&path).expect("remove config");
    let cfg = result.expect("load plugin directory config");
    assert_eq!(
        cfg.child.plugins_dir,
        Some(expand_tilde("~/neige second/plugins"))
    );
    assert_eq!(
        cfg.child.plugins_data_dir,
        Some(expand_tilde("~/neige second/plugin-data"))
    );
    let args = crate::calm_server_supervisor_config(&cfg).child_args;
    for (flag, value) in [
        ("--plugins-dir", "~/neige second/plugins"),
        ("--plugins-data-dir", "~/neige second/plugin-data"),
    ] {
        let expected = expand_tilde(value).display().to_string();
        assert!(
            args.windows(2).any(|pair| pair == [flag, &expected]),
            "missing {flag} {expected}: {args:?}"
        );
    }
    assert!(
        args.windows(2)
            .any(|pair| pair == ["--plugins-disabled", "example"])
    );
}

#[test]
fn config_omitted_plugin_dirs_leave_child_argv_unchanged() {
    let path = std::env::temp_dir().join(format!(
        "neige-app-no-plugin-dirs-{}.toml",
        std::process::id()
    ));
    fs::write(&path, "[child]\ndata_dir = \"/tmp/neige-second/data\"\n").expect("write config");
    let result = AppConfig::load(Some(&path));
    fs::remove_file(&path).expect("remove config");
    for cfg in [result.expect("load config"), AppConfig::starter(path)] {
        assert!(cfg.child.plugins_dir.is_none());
        assert!(cfg.child.plugins_data_dir.is_none());
        let args = crate::calm_server_supervisor_config(&cfg).child_args;
        assert!(
            !args
                .iter()
                .any(|arg| arg == "--plugins-dir" || arg == "--plugins-data-dir")
        );
        assert!(args.is_empty());
    }
}
