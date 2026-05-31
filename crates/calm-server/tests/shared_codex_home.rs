use std::path::{Path, PathBuf};

use calm_server::shared_codex_home::SharedCodexHome;

mod shared_codex_home {
    use super::*;

    fn shared_home(root: &tempfile::TempDir) -> SharedCodexHome {
        SharedCodexHome::new(
            root.path().join("codex-home"),
            root.path().join("codex-homes"),
        )
    }

    fn read_config(home: &SharedCodexHome) -> String {
        std::fs::read_to_string(home.path().join("config.toml")).expect("read config.toml")
    }

    fn parsed_config(home: &SharedCodexHome) -> toml::Value {
        let config = read_config(home);
        toml::from_str(&config).expect("config.toml must be valid TOML")
    }

    fn project_trust_level(config: &toml::Value, cwd: &Path) -> Option<String> {
        config
            .get("projects")
            .and_then(|v| v.get(cwd.to_string_lossy().as_ref()))
            .and_then(|v| v.get("trust_level"))
            .and_then(|v| v.as_str())
            .map(ToOwned::to_owned)
    }

    #[test]
    fn shared_config_writer_is_idempotent() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = shared_home(&root);
        let cwd = root.path().join("work");

        home.ensure_config_for_cwd(&cwd).expect("first write");
        home.ensure_config_for_cwd(&cwd).expect("second write");

        let config = parsed_config(&home);
        assert_eq!(
            project_trust_level(&config, &cwd).as_deref(),
            Some("trusted")
        );
    }

    #[test]
    fn shared_config_writer_preserves_existing_tables() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = shared_home(&root);
        std::fs::create_dir_all(home.path()).expect("mkdir home");
        let existing = r#"# user config
[mcp_servers.foo]
command = "/bin/foo"
args = ["--bar"]
"#;
        std::fs::write(home.path().join("config.toml"), existing).expect("write existing config");

        home.ensure_config_for_cwd(&root.path().join("work"))
            .expect("ensure config");

        let config = parsed_config(&home);
        let foo = config
            .get("mcp_servers")
            .and_then(|v| v.get("foo"))
            .expect("mcp_servers.foo");
        assert_eq!(
            foo.get("command").and_then(|v| v.as_str()),
            Some("/bin/foo")
        );
        assert_eq!(
            foo.get("args")
                .and_then(|v| v.as_array())
                .and_then(|v| v.first())
                .and_then(|v| v.as_str()),
            Some("--bar")
        );
        assert_eq!(
            config.get("approval_policy").and_then(|v| v.as_str()),
            Some("never")
        );
    }

    #[test]
    fn shared_config_writer_adds_multiple_project_blocks() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = shared_home(&root);
        let cwd_a = root.path().join("work-a");
        let cwd_b = root.path().join("work-b");

        home.ensure_config_for_cwd(&cwd_a).expect("write cwd a");
        home.ensure_config_for_cwd(&cwd_b).expect("write cwd b");

        let config = parsed_config(&home);
        assert_eq!(
            project_trust_level(&config, &cwd_a).as_deref(),
            Some("trusted")
        );
        assert_eq!(
            project_trust_level(&config, &cwd_b).as_deref(),
            Some("trusted")
        );
    }

    #[test]
    fn shared_home_seed_copies_auth_once() {
        let root = tempfile::tempdir().expect("tempdir");
        let host = tempfile::tempdir().expect("host codex tempdir");
        std::fs::write(host.path().join("auth.json"), r#"{"token":"first"}"#)
            .expect("write host auth");
        std::fs::write(host.path().join("config.toml"), "# host config\n")
            .expect("write host config");
        let home = shared_home(&root);

        home.seed_from(Some(host.path())).expect("seed once");
        assert_eq!(
            std::fs::read_to_string(home.path().join("auth.json")).expect("read auth"),
            r#"{"token":"first"}"#
        );

        std::fs::write(home.path().join("auth.json"), r#"{"token":"local"}"#)
            .expect("overwrite local auth");
        std::fs::write(host.path().join("auth.json"), r#"{"token":"second"}"#)
            .expect("overwrite host auth");
        home.seed_from(Some(host.path())).expect("seed twice");

        assert_eq!(
            std::fs::read_to_string(home.path().join("auth.json")).expect("read auth"),
            r#"{"token":"local"}"#
        );
    }

    #[test]
    fn shared_home_seed_does_not_overwrite_existing_non_auth_files() {
        let root = tempfile::tempdir().expect("tempdir");
        let host = tempfile::tempdir().expect("host codex tempdir");
        std::fs::write(host.path().join("auth.json"), r#"{"token":"host"}"#)
            .expect("write host auth");
        std::fs::write(host.path().join("config.toml"), "# host config\n")
            .expect("write host config");
        let home = shared_home(&root);
        std::fs::create_dir_all(home.path()).expect("mkdir home");
        std::fs::write(home.path().join("config.toml"), "# local config\n")
            .expect("write local config");

        home.seed_from(Some(host.path())).expect("seed");

        assert_eq!(
            std::fs::read_to_string(home.path().join("config.toml")).expect("read config"),
            "# local config\n"
        );
        assert_eq!(
            std::fs::read_to_string(home.path().join("auth.json")).expect("read auth"),
            r#"{"token":"host"}"#
        );
    }

    #[test]
    fn shared_config_writer_fills_existing_project_table_without_duplicate_header() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = shared_home(&root);
        std::fs::create_dir_all(home.path()).expect("mkdir home");
        let cwd = root.path().join("work");
        std::fs::write(
            home.path().join("config.toml"),
            format!(
                "[projects.{:?}]\n# user note inside project table\n",
                cwd.to_string_lossy()
            ),
        )
        .expect("write existing project table");

        home.ensure_config_for_cwd(&cwd).expect("ensure config");
        home.ensure_config_for_cwd(&cwd)
            .expect("ensure config again");

        let text = read_config(&home);
        let config: toml::Value = toml::from_str(&text).expect("must be valid TOML");
        assert_eq!(
            project_trust_level(&config, &cwd).as_deref(),
            Some("trusted")
        );
        assert!(text.contains("# user note inside project table\n"));
    }

    #[test]
    fn shared_config_writer_keeps_existing_top_level_values() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = shared_home(&root);
        std::fs::create_dir_all(home.path()).expect("mkdir home");
        std::fs::write(
            home.path().join("config.toml"),
            "approval_policy = \"never\"\nsandbox_mode = \"workspace-write\"\n",
        )
        .expect("write existing config");

        home.ensure_config_for_cwd(&root.path().join("work"))
            .expect("ensure config");

        let config = parsed_config(&home);
        assert_eq!(
            config.get("approval_policy").and_then(|v| v.as_str()),
            Some("never")
        );
        assert_eq!(
            config.get("sandbox_mode").and_then(|v| v.as_str()),
            Some("workspace-write")
        );
    }

    #[test]
    fn shared_home_exposes_legacy_parent_for_future_gc() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = shared_home(&root);

        assert_eq!(home.legacy_parent(), root.path().join("codex-homes"));
        assert_eq!(home.path(), root.path().join("codex-home"));
    }

    #[test]
    fn codex_runtime_state_files_include_memories_1_sqlite() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = shared_home(&root);
        let files = home.codex_runtime_state_files();

        assert!(files.contains(&PathBuf::from("memories_1.sqlite")));
        assert!(files.contains(&PathBuf::from("memories_1.sqlite-wal")));
        assert!(files.contains(&PathBuf::from("memories_1.sqlite-shm")));
    }

    #[test]
    fn ensure_config_for_cwd_escapes_quotes_and_backslashes() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = shared_home(&root);
        let cwd = root.path().join(r#"has"quote\and\slashes"#);

        home.ensure_config_for_cwd(&cwd).expect("ensure config");

        let config = parsed_config(&home);
        assert_eq!(
            project_trust_level(&config, &cwd).as_deref(),
            Some("trusted")
        );
    }

    #[test]
    fn seed_creates_home_directory() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = shared_home(&root);

        home.seed_from(None).expect("seed without host");

        assert!(home.path().is_dir());
    }

    #[test]
    fn ensure_config_for_cwd_writes_network_access_in_sandbox_block() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = shared_home(&root);

        home.ensure_config_for_cwd(&root.path().join("work"))
            .expect("ensure config");

        let config = parsed_config(&home);
        assert_eq!(
            config["sandbox_workspace_write"]["network_access"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn ensure_config_for_cwd_detects_dotted_table_form() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = shared_home(&root);
        std::fs::create_dir_all(home.path()).expect("mkdir home");
        std::fs::write(
            home.path().join("config.toml"),
            "sandbox_workspace_write.network_access = true\n",
        )
        .expect("write existing config");

        home.ensure_config_for_cwd(&root.path().join("work"))
            .expect("ensure config");

        let text = read_config(&home);
        let parsed: toml::Value = toml::from_str(&text).expect("must remain valid TOML");
        let block = parsed
            .get("sandbox_workspace_write")
            .and_then(|v| v.as_table())
            .expect("section present");
        assert_eq!(
            block.get("network_access").and_then(|v| v.as_bool()),
            Some(true)
        );
        let header_count = text.matches("[sandbox_workspace_write]").count();
        assert!(
            header_count <= 1,
            "must not duplicate bracket header alongside dotted key: {text}"
        );
    }

    #[test]
    fn ensure_config_for_cwd_does_not_false_positive_on_prefix() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = shared_home(&root);
        std::fs::create_dir_all(home.path()).expect("mkdir home");
        std::fs::write(
            home.path().join("config.toml"),
            "sandbox_workspace_writer.foo = 1\n",
        )
        .expect("write existing config");

        home.ensure_config_for_cwd(&root.path().join("work"))
            .expect("ensure config");

        let text = read_config(&home);
        let parsed: toml::Value = toml::from_str(&text).expect("must be valid TOML");
        assert_eq!(
            parsed["sandbox_workspace_write"]["network_access"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn ensure_config_for_cwd_ignores_dotted_key_inside_comment() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = shared_home(&root);
        std::fs::create_dir_all(home.path()).expect("mkdir home");
        std::fs::write(
            home.path().join("config.toml"),
            "# sandbox_workspace_write.network_access = true (disabled comment)\n",
        )
        .expect("write existing config");

        home.ensure_config_for_cwd(&root.path().join("work"))
            .expect("ensure config");

        let text = read_config(&home);
        let parsed: toml::Value = toml::from_str(&text).expect("must be valid TOML");
        let block = parsed
            .get("sandbox_workspace_write")
            .and_then(|v| v.as_table())
            .expect("section present");
        assert_eq!(
            block.get("network_access").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn ensure_config_for_cwd_handles_inline_comment_on_table_header() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = shared_home(&root);
        std::fs::create_dir_all(home.path()).expect("mkdir home");
        std::fs::write(
            home.path().join("config.toml"),
            "[mcp_servers.foo] # local server\ncommand = \"local-mcp\"\n",
        )
        .expect("write existing config");

        home.ensure_config_for_cwd(&root.path().join("work"))
            .expect("ensure config");

        let text = read_config(&home);
        let parsed: toml::Value = toml::from_str(&text).expect("must be valid TOML");
        assert_eq!(
            parsed.get("approval_policy").and_then(|v| v.as_str()),
            Some("never")
        );
        let foo = parsed
            .get("mcp_servers")
            .and_then(|v| v.get("foo"))
            .expect("mcp_servers.foo");
        assert!(
            foo.get("approval_policy").is_none(),
            "must NOT inherit approval_policy: {text}"
        );
        assert_eq!(
            foo.get("command").and_then(|v| v.as_str()),
            Some("local-mcp")
        );
    }

    #[test]
    fn ensure_config_for_cwd_preserves_user_comments() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = shared_home(&root);
        std::fs::create_dir_all(home.path()).expect("mkdir home");
        std::fs::write(
            home.path().join("config.toml"),
            "# User says hello\n[mcp_servers.foo]\ncommand = \"x\"  # important\n",
        )
        .expect("write existing config");

        home.ensure_config_for_cwd(&root.path().join("work"))
            .expect("ensure config");

        let text = read_config(&home);
        assert!(text.contains("# User says hello"));
        assert!(text.contains("# important"));
    }

    #[test]
    fn ensure_config_for_cwd_does_not_overwrite_user_top_level_value() {
        let root = tempfile::tempdir().expect("tempdir");
        let home = shared_home(&root);
        std::fs::create_dir_all(home.path()).expect("mkdir home");
        std::fs::write(
            home.path().join("config.toml"),
            "approval_policy = \"on-failure\"\n",
        )
        .expect("write existing config");

        home.ensure_config_for_cwd(&root.path().join("work"))
            .expect("ensure config");

        let text = read_config(&home);
        let parsed: toml::Value = toml::from_str(&text).expect("must be valid TOML");
        assert_eq!(
            parsed.get("approval_policy").and_then(|v| v.as_str()),
            Some("on-failure"),
            "must preserve user override: {text}",
        );
    }
}
