//! Project-code effects require the original input/role owner after local resource waits.
use super::*;
use serde_json::json;
use std::path::{Path, PathBuf};

/// Re-exec before creating a runtime: direnv trust and all fixture temp paths are private.
fn isolated(test: &str) -> bool {
    if std::env::var_os("ACP_ADMISSION_ENV_CHILD").is_some() {
        return false;
    }
    std::fs::create_dir_all("_local/tmp").unwrap();
    let root = tempfile::tempdir_in("_local/tmp").unwrap();
    let root = root.path().canonicalize().unwrap();
    let module = module_path!()
        .strip_prefix(concat!(env!("CARGO_CRATE_NAME"), "::"))
        .unwrap();
    let name = format!("{module}::{test}");
    std::fs::create_dir_all(root.join("tmp")).unwrap();
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([&name, "--exact", "--nocapture", "--test-threads=1"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", &root)
        .env("TMPDIR", root.join("tmp"))
        .env("XDG_CONFIG_HOME", root.join("xdg"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("DIRENV_CONFIG", root.join("direnv"))
        .env("ACP_ADMISSION_ENV_CHILD", "1")
        .current_dir(&root);
    // The existing execution fixture consumes only nextest's explicitly prepared helper.
    for key in ["NEXTEST", "NEIGE_TEST_PROC_SUPERVISOR_BIN"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let output = command.output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success()
            && stdout.lines().any(|line| {
                line.starts_with("test result: ok. 1 passed; 0 failed; 0 ignored;")
            }),
        "isolated test must select and pass exactly once\n{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    true
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

/// Authorize only this synthetic nearest rc, using the child-owned trust directory.
fn marker_rc(source: &Path) -> PathBuf {
    std::fs::write(
        source.join(".envrc"),
        "printf evaluated >> \"$PWD/environment-evaluated\"\n",
    )
    .unwrap();
    let output = std::process::Command::new("direnv")
        .arg("allow")
        .arg(source)
        .env_clear()
        .envs(crate::workspace_service::environment::directory_base_environment())
        .output()
        .unwrap();
    assert!(output.status.success(), "synthetic rc must be trusted");
    source.join("environment-evaluated")
}

async fn pause_environment(fx: &Fixture) -> crate::test_seams::PausePoint {
    // Use the actual workspace owner's selected source, including a derived checkout mapping.
    let source = crate::workspace_service::environment::source_for_track(
        fx.owner.repo.as_ref(),
        &fx.owner.track,
        fx.dir.path(),
    )
    .await
    .unwrap();
    let pause = crate::test_seams::PausePoint::default();
    crate::test_seams::install_pause_for_test(
        "workspace-environment-before-ready",
        source.to_str().unwrap(),
        pause.clone(),
    );
    pause
}

struct ReleaseOnDrop(crate::test_seams::PausePoint);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.release.notify_one();
    }
}

async fn no_receipt(fx: &Fixture) {
    let count: i64 =
        sqlx::query_scalar("SELECT count(*) FROM acp_submissions WHERE worker_session_id=?1")
            .bind(&fx.owner.worker)
            .fetch_one(fx.owner.repo.pool())
            .await
            .unwrap();
    assert_eq!(count, 0, "refusal must precede the native receipt");
    assert_eq!(fx.prompts(), 0);
}

#[test]
fn acp_per_turn_project_eval_rechecks_canonical_source_after_queue_wait() {
    if isolated("acp_per_turn_project_eval_rechecks_canonical_source_after_queue_wait") {
        return;
    }
    runtime().block_on(async {
        // Prove real first-None-native and bound-native continuation both work with this profile.
        let positive = Fixture::with_provider(AgentProvider::Grok, "reply", false).await;
        let positive_marker = marker_rc(positive.dir.path());
        positive.dispatch().await.unwrap();
        positive.wait_end().await;
        let next = positive.claim_successor().await;
        positive.dispatch_submission(&next).await;
        positive.wait_end().await;
        assert_eq!(positive.prompts(), 2);
        assert_eq!(
            std::fs::read_to_string(positive_marker).unwrap(),
            "evaluatedevaluated"
        );
        positive.session.shutdown().await.unwrap();

        let fx = Arc::new(Fixture::with_provider(AgentProvider::Grok, "reply", false).await);
        let marker = marker_rc(fx.dir.path());
        let pause = pause_environment(&fx).await;
        let _release_on_failure = ReleaseOnDrop(pause.clone());
        let dispatch = tokio::spawn({
            let fx = fx.clone();
            async move { fx.dispatch().await }
        });
        let entered = tokio::time::timeout(Duration::from_secs(20), pause.entered.notified()).await;
        if entered.is_err() {
            // Never strand a blocking worker when the seam is absent or its key changes.
            pause.release.notify_one();
        }
        entered.expect("canonical input must reach the local environment resource wait");
        assert!(!marker.exists());
        assert!(fx.source.current_principal().is_ok());
        fx.auth.sessions.remove(&fx.login).unwrap();
        assert!(fx.source.current_principal().is_err());
        pause.release.notify_one();
        let report = tokio::time::timeout(Duration::from_secs(20), dispatch)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(matches!(
            report.report.delivery(),
            InputDelivery::NotDispatched(_)
        ));
        fx.wait_driver_finished().await;
        no_receipt(&fx).await;
        fx.session.shutdown().await.unwrap();
        assert!(
            !marker.exists(),
            "revoked canonical source executed project code after waiting"
        );
    });
}

#[test]
fn acp_resident_project_eval_rechecks_role_after_slot_and_local_wait() {
    if isolated("acp_resident_project_eval_rechecks_role_after_slot_and_local_wait") {
        return;
    }
    runtime().block_on(async {
        // A real first attachment has no bound native id yet; its original role must admit it.
        let positive = Fixture::with_provider(AgentProvider::Antigravity, "reply", false).await;
        let positive_marker = marker_rc(positive.dir.path());
        positive.dispatch().await.unwrap();
        positive.wait_end().await;
        assert_eq!(positive.prompts(), 1);
        assert_eq!(
            std::fs::read_to_string(positive_marker).unwrap(),
            "evaluated"
        );
        positive.session.shutdown().await.unwrap();

        let fx = Arc::new(Fixture::with_provider(AgentProvider::Antigravity, "reply", false).await);
        let marker = marker_rc(fx.dir.path());
        let slot = fx
            .session
            .host()
            .resident_slot(&AgentProvider::Antigravity, &fx.owner.track, fx.dir.path())
            .unwrap();
        let startup = slot.lock().await;
        assert!(
            startup.is_none(),
            "the refused owner has no existing generation"
        );
        let pause = pause_environment(&fx).await;
        let _release_on_failure = ReleaseOnDrop(pause.clone());
        let dispatch = tokio::spawn({
            let fx = fx.clone();
            async move { fx.dispatch().await }
        });
        drop(startup);
        let entered = tokio::time::timeout(Duration::from_secs(20), pause.entered.notified()).await;
        if entered.is_err() {
            pause.release.notify_one();
        }
        entered.expect("resident startup must pass its slot and reach the local resource wait");
        assert!(
            slot.try_lock().is_err(),
            "attachment retains the actual startup slot"
        );
        assert!(!marker.exists());
        // Revoke the existing role's current-card owner, without altering its source login.
        (fx.owner.repo.db())
            .writer()
            .execute(
                sqlx::query("UPDATE cards SET session_id=NULL WHERE id=?1").bind(&fx.owner.card),
            )
            .await
            .unwrap();
        assert!(fx.source.current_principal().is_ok());
        pause.release.notify_one();
        assert!(
            tokio::time::timeout(Duration::from_secs(20), dispatch)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        fx.wait_driver_finished().await;
        no_receipt(&fx).await;
        fx.session.shutdown().await.unwrap();

        assert!(
            !marker.exists(),
            "revoked resident role executed project code before its gate"
        );
    });
}

#[test]
fn project_cwd_catalog_uses_registered_control_without_evaluating_rc() {
    if isolated("project_cwd_catalog_uses_registered_control_without_evaluating_rc") {
        return;
    }
    runtime().block_on(async {
        let root = tempfile::tempdir().unwrap();
        let cwd = root.path().canonicalize().unwrap();
        let marker = marker_rc(&cwd);
        std::fs::write(cwd.join("scenario"), "reply").unwrap();
        let host = AcpPlannerHost::new(
            Some(serde_json::from_value(json!({"agents":[{
                "provider":"grok", "command":"/usr/bin/python3",
                "args":["-u",concat!(env!("CARGO_MANIFEST_DIR"),"/tests/fixtures/acp_planner/agent.py")],
                "env":{
                    "ACP_FIXTURE_ROOT":cwd, "ACP_FIXTURE_IDENTITY":"meta",
                    "ACP_FIXTURE_VERSION":"1.0.70", "ACP_FIXTURE_AUTH_METHODS":"cached_token",
                    "ACP_FIXTURE_PROBE_KEYS":"GROK_HOME", "GROK_HOME":"/synthetic/operator-account"
                }, "min_agent_version":"1.0.70"
            }]})).unwrap()),
            &cwd, cwd.join("unused-shim"), cwd.join("mcp.sock"), Default::default(),
        ).unwrap();
        let catalog = host.discover_catalog(&AgentProvider::Grok, Some(&cwd)).await;
        assert!(catalog.models.iter().any(|model| model.id == "fixture/model-a"));
        let frames: Vec<serde_json::Value> = std::fs::read_to_string(cwd.join("requests.jsonl"))
            .unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect();
        let new = frames.iter().find(|frame| frame["method"] == "session/new").unwrap();
        assert_eq!(new["params"]["cwd"], cwd.to_str().unwrap());
        assert_eq!(new["params"]["mcpServers"], json!([]));
        assert!(frames.iter().any(|frame| frame["method"] == "initialize"));
        assert!(frames.iter().any(|frame| frame["method"] == "authenticate"
            && frame["params"] == json!({"methodId":"cached_token"})));
        assert!(!frames.iter().any(|frame| frame["method"] == "session/prompt"));
        let environment: serde_json::Value = serde_json::from_str(
            std::fs::read_to_string(cwd.join("environment.jsonl")).unwrap().lines().next().unwrap(),
        ).unwrap();
        assert_eq!(environment["cwd"], cwd.to_str().unwrap());
        use sha2::Digest as _;
        assert_eq!(environment["probe"]["GROK_HOME"],
            format!("{:x}", sha2::Sha256::digest(b"/synthetic/operator-account")));
        assert_eq!(environment["NEIGE_MCP_TOKEN"], false);
        assert!(!marker.exists(), "a metadata catalog probe executed project code");
    });
}
