use super::*;

struct Fixture {
    root: tempfile::TempDir,
    seed: HomeSeed,
    native: NativeMcp,
    _listener: std::os::unix::net::UnixListener,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        let auth = root.path().join("auth.json");
        std::fs::write(&config, "model='fake'\n").unwrap();
        std::fs::write(&auth, r#"{"token":"original"}"#).unwrap();
        let socket = root.path().join("mcp.sock");
        let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        Self {
            seed: HomeSeed::read(&config, &auth).unwrap(),
            native: NativeMcp {
                socket,
                card_token: "fake-token".into(),
                plugin_tools: Vec::new(),
            },
            root,
            _listener: listener,
        }
    }
}

#[test]
fn dedicated_codex_home_exact_replay_repeats_failed_publication_barrier() {
    let _reset = io::faults::reset_on_drop();
    let f = Fixture::new();
    let root = f.root.path().join("private");
    let home = PrivateHome::open(&root).unwrap();
    io::faults::fail_sync(Some(root.clone()));
    assert!(
        home.prepare("owned", "request", &f.seed, &f.native)
            .is_err()
    );
    let authentication = root.join("owned/home/auth.json");
    assert!(
        authentication.is_file(),
        "publication must be visible despite failed barrier"
    );
    std::fs::write(&authentication, r#"{"token":"refreshed"}"#).unwrap();
    assert!(
        home.prepare("owned", "request", &f.seed, &f.native)
            .is_err(),
        "visible exact replay must not bypass a still-failing parent fsync"
    );
    io::faults::fail_sync(None);
    home.prepare("owned", "request", &f.seed, &f.native)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(authentication).unwrap(),
        r#"{"token":"refreshed"}"#
    );
}

#[test]
fn dedicated_codex_home_concurrent_winner_repeats_publication_barrier() {
    let _reset = io::faults::reset_on_drop();
    let f = Fixture::new();
    let root = f.root.path().join("private");
    let home = PrivateHome::open(&root).unwrap();
    let winner = PrivateHome::open(&root).unwrap();
    let seed = f.seed.clone();
    let native = NativeMcp {
        socket: f.native.socket.clone(),
        card_token: f.native.card_token.clone(),
        plugin_tools: Vec::new(),
    };
    let sync_root = root.clone();
    io::faults::publish_once(move || {
        // A second preparer publishes after this preparer's existence check.
        std::thread::spawn(move || winner.prepare("owned", "request", &seed, &native).unwrap())
            .join()
            .unwrap();
        std::fs::write(
            sync_root.join("owned/home/auth.json"),
            r#"{"token":"refreshed"}"#,
        )
        .unwrap();
        io::faults::fail_sync(Some(sync_root));
    });
    assert!(
        home.prepare("owned", "request", &f.seed, &f.native)
            .is_err(),
        "concurrent EEXIST adoption must complete its own barrier"
    );
    io::faults::fail_sync(None);
    home.prepare("owned", "request", &f.seed, &f.native)
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("owned/home/auth.json")).unwrap(),
        r#"{"token":"refreshed"}"#
    );
}

#[test]
fn dedicated_codex_home_root_creation_and_reopen_repeat_ancestry_barriers() {
    let _reset = io::faults::reset_on_drop();
    let f = Fixture::new();
    let root = f.root.path().join("new-parent/private");
    io::faults::fail_sync(Some(f.root.path().to_path_buf()));
    assert!(
        PrivateHome::open(&root).is_err(),
        "creating a root must make its containing entries durable"
    );
    assert!(root.is_dir());
    assert!(
        PrivateHome::open(&root).is_err(),
        "reopen must repeat a formerly failed ancestor barrier"
    );
    io::faults::fail_sync(None);
    PrivateHome::open(&root).unwrap();
}

#[test]
fn dedicated_codex_plugin_grants_are_explicit_and_do_not_enable_network() {
    let mut f = Fixture::new();
    f.native.plugin_tools = vec!["plugin.research_lookup".into()];
    let home = PrivateHome::open(&f.root.path().join("grant-home")).unwrap();
    let receipt = home
        .prepare("grant", "request", &f.seed, &f.native)
        .unwrap();
    let text = std::fs::read_to_string(receipt.home.join("config.toml")).unwrap();
    let doc: toml_edit::DocumentMut = text.parse().unwrap();
    let tools: Vec<_> = doc["mcp_servers"]["calm"]["enabled_tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        tools,
        vec![
            "calm.task.complete",
            "calm.task.fail",
            "calm.report.read",
            "calm.plan.list",
            "plugin.research_lookup"
        ]
    );
    assert_eq!(
        doc["permissions"][super::policy::DELIVERY_PROFILE]["network"]["enabled"].as_bool(),
        Some(false)
    );
    assert_eq!(doc["web_search"].as_str(), Some("disabled"));
    f.native.plugin_tools.push("plugin.research_detail".into());
    assert!(
        home.prepare("grant", "request", &f.seed, &f.native)
            .is_err(),
        "a prepared private home must reject a changed grant set"
    );
}
