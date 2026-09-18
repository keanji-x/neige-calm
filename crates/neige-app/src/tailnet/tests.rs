use super::*;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

fn fixture() -> (tempfile::TempDir, TailnetConfig) {
    let dir = tempfile::tempdir().unwrap();
    let binary = dir.path().join("helper");
    std::fs::write(
        &binary,
        r#"#!/usr/bin/python3
import json, os, pathlib, time
pathlib.Path('spawn.json').write_text(json.dumps(dict(os.environ)))
pathlib.Path('started').write_text(str(os.getpid()))
time.sleep(60)
"#,
    )
    .unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let cfg = TailnetConfig {
        binary,
        state_dir: dir.path().join("private"),
        state_dir_inherits_data: false,
        hostname: "fixture".into(),
        enrollment_config: None,
    };
    (dir, cfg)
}
async fn wait_file(path: PathBuf) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !path.exists() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn tailnet_spawn_env_allowlist() {
    const SENTINEL: &str = "NEIGE_TAILNET_TEST_SECRET";
    if std::env::var_os(SENTINEL).is_none() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "tailnet::tests::tailnet_spawn_env_allowlist",
                "--nocapture",
            ])
            .env(SENTINEL, "fixture-secret")
            .env("TS_AUTHKEY", "fixture-key")
            .env("HTTPS_PROXY", "http://invalid.fixture:1")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let (_dir, cfg) = fixture();
    let manager = TailnetManager::start(cfg.clone()).unwrap();
    manager.action(TailnetAction::Enable).await.unwrap();
    wait_file(cfg.state_dir.join("spawn.json")).await;
    manager.shutdown().await.unwrap();
    let env: std::collections::BTreeMap<String, String> =
        serde_json::from_slice(&std::fs::read(cfg.state_dir.join("spawn.json")).unwrap()).unwrap();
    assert!(
        !env.contains_key(SENTINEL),
        "server credential reached child"
    );
    assert!(
        !env.contains_key("TS_AUTHKEY"),
        "ambient enrollment credential reached child"
    );
    assert!(
        !env.contains_key("HTTPS_PROXY"),
        "ambient proxy reached child"
    );
    assert_eq!(env.get("HOME"), Some(&cfg.state_dir.display().to_string()));
}
#[tokio::test]
async fn tailnet_disable_persists_and_retains_identity() {
    let (_dir, cfg) = fixture();
    let manager = TailnetManager::start(cfg.clone()).unwrap();
    assert!(
        !manager
            .action(TailnetAction::Status)
            .await
            .unwrap()
            .status
            .desired_enabled
    );
    manager.action(TailnetAction::Enable).await.unwrap();
    wait_file(cfg.state_dir.join("started")).await;
    let node = cfg.state_dir.join("node");
    std::fs::create_dir(&node).unwrap();
    std::fs::write(node.join("identity"), "private-fixture").unwrap();
    let status = manager.action(TailnetAction::Disable).await.unwrap().status;
    assert!(!status.desired_enabled && !status.process_running);
    assert_eq!(
        std::fs::read_to_string(node.join("identity")).unwrap(),
        "private-fixture"
    );
    assert!(!storage::load(&cfg.state_dir).unwrap().desired_enabled);
    assert_eq!(
        std::fs::metadata(cfg.state_dir.join("desired.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    manager.shutdown().await.unwrap();
}
#[tokio::test]
async fn tailnet_narrow_socket_rejects_admin_actions_and_wrong_version() {
    let (_dir, cfg) = fixture();
    let manager = TailnetManager::start(cfg.clone()).unwrap();
    for request in [
        b"{\"version\":1,\"action\":\"restart\"}\n".as_slice(),
        b"{\"version\":2,\"action\":\"enable\"}\n",
        b"{\"version\":1,\"action\":\"enable\",\"upstream\":\"http://evil\"}\n",
    ] {
        let mut stream = UnixStream::connect(cfg.socket()).await.unwrap();
        stream.write_all(request).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        assert!(response.is_empty());
    }
    let status = TailnetClient::new(cfg.socket())
        .request(TailnetAction::Status)
        .await
        .unwrap();
    assert!(!status.status.desired_enabled);
    manager.shutdown().await.unwrap();
}
#[test]
fn tailnet_configuration_rejects_shared_authority() {
    let (_dir, mut cfg) = fixture();
    assert!(cfg.validate(&[]).is_ok());
    assert!(
        cfg.validate(&["--mobile-access-config=/fixture/funnel".into()])
            .is_err()
    );
    cfg.state_dir = PathBuf::from("relative");
    assert!(cfg.validate(&[]).is_err());
    cfg.state_dir = PathBuf::from(format!("/{}", "x".repeat(105)));
    assert!(cfg.validate(&[]).is_err());
}
#[tokio::test]
async fn tailnet_crashes_trip_circuit_without_changing_desired_state() {
    let (_dir, cfg) = fixture();
    std::fs::write(&cfg.binary, "#!/bin/sh\nexit 1\n").unwrap();
    let manager = TailnetManager::start(cfg.clone()).unwrap();
    manager.action(TailnetAction::Enable).await.unwrap();
    for _ in 0..6 {
        tokio::time::sleep(Duration::from_millis(30)).await;
        let mut state = manager.state.lock().await;
        manager.reconcile(&mut state).await;
        state.next_start = Instant::now();
        manager.reconcile(&mut state).await;
    }
    let mut state = manager.state.lock().await;
    manager.reconcile(&mut state).await;
    assert!(state.failed && state.child.is_none());
    assert!(state.desired.desired_enabled);
    drop(state);
    manager.shutdown().await.unwrap();
}

#[tokio::test]
async fn tailnet_pins_binary_across_release_switch_and_kernel_restart() {
    use std::os::unix::fs::symlink;
    let (dir, mut cfg) = fixture();
    let original = cfg.binary.clone();
    let current = dir.path().join("current-helper");
    symlink(&original, &current).unwrap();
    cfg.binary = current.clone();
    let manager = TailnetManager::start(cfg.clone()).unwrap();
    manager.action(TailnetAction::Enable).await.unwrap();
    wait_file(cfg.state_dir.join("started")).await;
    let before = manager.state.lock().await.child.as_ref().unwrap().id();
    let alternate = dir.path().join("replacement");
    std::fs::write(&alternate, "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(&alternate, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::remove_file(&current).unwrap();
    symlink(&alternate, &current).unwrap();
    assert_eq!(manager.pinned_binary.as_ref(), Some(&original));
    let kernel = crate::Supervisor::new(crate::SupervisorConfig {
        name: "kernel-fixture".into(),
        child_bin: "/bin/sleep".into(),
        child_cwd: None,
        child_args: vec!["60".into()],
        child_envs: vec![],
        restart_delay: Duration::from_millis(10),
        stop_grace: Duration::from_millis(100),
        calm_listen: None,
        persist_identity_to: None,
        boot_plugin_budget: Duration::ZERO,
    });
    let task = tokio::spawn(kernel.clone().run());
    kernel.wait_for_spawn(Duration::from_secs(3)).await.unwrap();
    kernel.restart().await.unwrap();
    kernel.wait_for_spawn(Duration::from_secs(3)).await.unwrap();
    assert_eq!(
        manager.state.lock().await.child.as_ref().unwrap().id(),
        before
    );
    assert!(
        manager
            .state
            .lock()
            .await
            .child
            .as_mut()
            .unwrap()
            .try_wait()
            .unwrap()
            .is_none()
    );
    kernel.shutdown().await;
    task.await.unwrap();
    manager.shutdown().await.unwrap();
}

#[test]
fn tailnet_state_backup_requires_no_active_writer_and_never_restores() {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let (_dir, cfg) = fixture();
    let _lock = storage::lock_directory(&cfg.state_dir).unwrap();
    storage::backup_for_binary(&cfg.state_dir, &cfg.binary).unwrap();
    let node = cfg.state_dir.join("node");
    std::fs::create_dir(&node).unwrap();
    std::fs::write(node.join("identity"), "old-identity").unwrap();
    std::fs::write(&cfg.binary, "changed-binary").unwrap();
    let writer = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(cfg.state_dir.join("node.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(writer.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
    assert!(storage::backup_for_binary(&cfg.state_dir, &cfg.binary).is_err());
    drop(writer);
    storage::backup_for_binary(&cfg.state_dir, &cfg.binary).unwrap();
    let backup = std::fs::read_dir(cfg.state_dir.join("backups"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(
        std::fs::read_to_string(backup.join("node/identity")).unwrap(),
        "old-identity"
    );
    std::fs::write(node.join("identity"), "new-identity").unwrap();
    storage::backup_for_binary(&cfg.state_dir, &cfg.binary).unwrap();
    assert_eq!(
        std::fs::read_to_string(node.join("identity")).unwrap(),
        "new-identity"
    );
}

#[test]
fn tailnet_cli_data_override_isolated_but_explicit_node_path_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let mut config = crate::config::AppConfig::starter(dir.path().join("config.toml"));
    config.apply_serve_overrides(crate::config::ServeOverrides {
        calm_data_dir: Some(dir.path().join("isolated")),
        ..Default::default()
    });
    assert_eq!(
        config.tailnet.as_ref().unwrap().state_dir,
        dir.path().join("isolated/tailnet")
    );
    let explicit = config.tailnet.as_mut().unwrap();
    explicit.state_dir_inherits_data = false;
    config.apply_serve_overrides(crate::config::ServeOverrides {
        calm_data_dir: Some(dir.path().join("another")),
        ..Default::default()
    });
    assert_eq!(
        config.tailnet.as_ref().unwrap().state_dir,
        dir.path().join("isolated/tailnet")
    );
}

#[tokio::test]
async fn tailnet_disabled_cleanup_passes_only_config_reference_without_node_mode() {
    let (_dir, mut cfg) = fixture();
    cfg.enrollment_config = Some(cfg.state_dir.join("not-readable-by-rust.json"));
    std::fs::write(
        &cfg.binary,
        r#"#!/usr/bin/python3
import json, pathlib, sys
pathlib.Path('cleanup-args.json').write_text(json.dumps(sys.argv[1:]))
"#,
    )
    .unwrap();
    let manager = TailnetManager::start(cfg.clone()).unwrap();
    wait_file(cfg.state_dir.join("cleanup-args.json")).await;
    manager.shutdown().await.unwrap();
    let args: Vec<String> =
        serde_json::from_slice(&std::fs::read(cfg.state_dir.join("cleanup-args.json")).unwrap())
            .unwrap();
    assert!(args.iter().any(|a| a == "--cleanup-only"));
    let path = args
        .iter()
        .position(|a| a == "--enrollment-config")
        .unwrap()
        + 1;
    assert_eq!(
        args[path],
        cfg.enrollment_config.unwrap().display().to_string()
    );
    assert!(!cfg.state_dir.join("node").exists());
}

#[tokio::test]
async fn tailnet_disabled_host_reports_pending_cleanup_without_node() {
    use calm_types::enrollment::{EnrollmentAction, EnrollmentCommand};
    let (_dir, mut cfg) = fixture();
    cfg.binary = cfg.binary.with_file_name("real-helper");
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tailnet");
    let built = std::process::Command::new("go")
        .args(["build", "-p", "2", "-tags", "ts_omit_logtail", "-o"])
        .arg(&cfg.binary)
        .arg(".")
        .env("GOMAXPROCS", "2")
        .current_dir(source)
        .output()
        .unwrap();
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    std::fs::create_dir(&cfg.state_dir).unwrap();
    std::fs::set_permissions(&cfg.state_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let ledger = cfg.state_dir.join("enrollment-ledger.json");
    std::fs::write(&ledger,serde_json::json!({"schemaVersion":1,"records":[{
        "enrollmentId":"pending","bindingHash":"a".repeat(64),"keyId":"key-id","expires":"2099-01-01T00:00:00Z","state":"cleanup","deadline":1
    }]}).to_string()).unwrap();
    std::fs::set_permissions(&ledger, std::fs::Permissions::from_mode(0o600)).unwrap();
    cfg.enrollment_config = Some(cfg.state_dir.join("absent.json"));
    let manager = TailnetManager::start(cfg.clone()).unwrap();
    manager.action(TailnetAction::Status).await.unwrap();
    let request = || EnrollmentCommand {
        action: EnrollmentAction::Status,
        enrollment_id: "status".into(),
        generation: "generation".into(),
        deadline: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 8000,
    };
    let result = TailnetClient::new(cfg.socket()).enrollment(request()).await;
    let result = result.unwrap_or_else(|e| panic!("disabled status rejected: {e}"));
    assert_eq!(result.pending_cleanup, 1);
    assert!(result.detail.contains("2099-01-01T00:00:00Z"));
    assert!(result.auth_key.is_empty());
    let before = std::fs::read(&ledger).unwrap();
    assert!(
        TailnetClient::new(cfg.socket())
            .enrollment(request())
            .await
            .is_ok()
    );
    assert_eq!(
        std::fs::read(&ledger).unwrap(),
        before,
        "status rewrote the ledger"
    );
    std::fs::write(&ledger, "corrupt fixture ledger").unwrap();
    assert!(
        TailnetClient::new(cfg.socket())
            .enrollment(request())
            .await
            .is_err(),
        "corrupt status fabricated no pending keys"
    );
    manager.shutdown().await.unwrap();
    for name in ["node", "helper.sock", "state-version"] {
        assert!(!cfg.state_dir.join(name).exists(), "status started {name}");
    }
}

#[tokio::test]
async fn tailnet_stopped_status_preserves_cleanup_process_failure() {
    use calm_types::enrollment::{EnrollmentAction, EnrollmentCommand};
    let (_dir, mut cfg) = fixture();
    cfg.enrollment_config = Some(cfg.state_dir.join("unused.json"));
    std::fs::write(&cfg.binary,r#"#!/usr/bin/python3
import json, sys
if '--cleanup-only' in sys.argv:
    sys.exit(2)
if '--cleanup-status' in sys.argv:
    print(json.dumps({'version':2,'pendingCleanup':1,'detail':'Retained key; returned expiry 2099-01-01T00:00:00Z'}))
    sys.exit(0)
sys.exit(3)
"#).unwrap();
    let manager = TailnetManager::start(cfg.clone()).unwrap();
    manager.action(TailnetAction::Status).await.unwrap();
    let result = TailnetClient::new(cfg.socket())
        .enrollment(EnrollmentCommand {
            action: EnrollmentAction::Status,
            enrollment_id: "status".into(),
            generation: "g".into(),
            deadline: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_millis() as i64
                + 8000,
        })
        .await;
    manager.shutdown().await.unwrap();
    let result = result.unwrap();
    assert_eq!(result.pending_cleanup, 1);
    assert!(result.detail.contains("Cleanup helper failed"));
    assert!(result.detail.contains("2099-01-01T00:00:00Z"));
}

#[tokio::test]
async fn tailnet_app_cancel_overtakes_partial_create_without_cloud_post() {
    use calm_types::enrollment::{
        EnrollmentAction, EnrollmentCommand, EnrollmentRequest, EnrollmentResponse,
    };
    let (dir, cfg) = fixture();
    let helper = dir.path().join("go-fixture");
    let output = std::process::Command::new("go")
        .args(["test", "-c", "-p", "2", "-tags", "ts_omit_logtail", "-o"])
        .arg(&helper)
        .arg(".")
        .env("GOMAXPROCS", "2")
        .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tailnet"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::write(
        &cfg.binary,
        format!(
            "#!/bin/sh\nexec '{}' -test.run '^TestEnrollmentAppControlFixture$' -- \"$@\"\n",
            helper.display()
        ),
    )
    .unwrap();
    let manager = TailnetManager::start(cfg.clone()).unwrap();
    manager.action(TailnetAction::Enable).await.unwrap();
    wait_file(cfg.helper_socket()).await;
    let make = |action| EnrollmentCommand {
        action,
        enrollment_id: "overtaken".into(),
        generation: "original-generation".into(),
        deadline: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 8000,
    };
    let bytes = serde_json::to_vec(&EnrollmentRequest {
        version: 2,
        command: make(EnrollmentAction::Create),
    })
    .unwrap();
    let mut original = UnixStream::connect(cfg.socket()).await.unwrap();
    original.write_all(&bytes[..bytes.len() / 2]).await.unwrap();
    let cancelled = TailnetClient::new(cfg.socket())
        .enrollment(make(EnrollmentAction::Cancel))
        .await;
    original.write_all(&bytes[bytes.len() / 2..]).await.unwrap();
    original.write_all(b"\n").await.unwrap();
    let mut response = Vec::new();
    BufReader::new(original)
        .read_until(b'\n', &mut response)
        .await
        .unwrap();
    manager.shutdown().await.unwrap();
    assert!(cancelled.is_ok(), "cancel acknowledgement missing");
    let response: EnrollmentResponse = serde_json::from_slice(&response).unwrap();
    assert!(
        response
            .error
            .as_deref()
            .is_some_and(|e| e.contains("already admitted or cancelled")),
        "late create lacked the production issuer fence"
    );
    assert!(
        !cfg.state_dir.join("cloud-post").exists(),
        "overtaken handler sent a key POST"
    );
}
