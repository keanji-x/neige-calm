//! Exercise the real session wiring before releasing a project environment to a child.
use super::*;
use crate::db::{RepoOutOfDomain, RepoSyncDomainRaw};
use crate::harness::profile::{HarnessProfile, PlannerBinding};
use crate::harness::wiring::{PlannerRow, PlannerWiring};
use crate::model::{CardRole, NewArea, NewCard, NewTrack, RequestTheme, new_id};
use crate::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use workspace_runtime::execution::ExecutionId;

#[tokio::test]
async fn workspace_commands_keep_wired_model_proxy() {
    if std::env::var_os("CLAUDE_PROXY_TEST_CHILD").is_none() {
        std::fs::create_dir_all("_local/tmp").unwrap();
        let home = tempfile::tempdir_in("_local/tmp").unwrap();
        let home = std::fs::canonicalize(home.path()).unwrap();
        let name = crate::test_support::exact_test_name(
            module_path!(),
            stringify!(workspace_commands_keep_wired_model_proxy),
        );
        let output = Command::new(std::env::current_exe().unwrap())
            .args([name.as_str(), "--exact", "--nocapture", "--test-threads=1"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_DATA_HOME", home.join("data"))
            .env("TMPDIR", &home)
            .env("HTTP_PROXY", "http://ambient-model-proxy:8181")
            .env("https_proxy", "http://ambient-model-proxy:8182")
            .env("CLAUDE_PROXY_TEST_CHILD", "1")
            .output()
            .unwrap();
        crate::test_support::assert_isolated_test_passed(&output);
        return;
    }

    let root = tempfile::tempdir().unwrap();
    let repo = Arc::new(
        crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
            .await
            .unwrap(),
    );
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "proxy".into(),
                color: "#111111".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            area_id: area.id,
            title: "proxy".into(),
            cwd: root.path().display().to_string(),
            sort: None,
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let track_id = track.id.clone();
    let card_id = crate::db::write_in_tx_typed(repo.as_ref(), move |tx| {
        Box::pin(async move {
            let card = crate::db::sqlite::card_create_with_id_tx(
                tx,
                new_id(),
                NewCard {
                    track_id,
                    title: None,
                    kind: "codex".into(),
                    sort: None,
                    payload: serde_json::json!({"harness_profile":"assistant", "assistant_provider":"claude"}),
                },
                CardRole::Assistant,
                false,
                &crate::card_role_cache::CardRoleCache::new(),
            )
            .await?;
            crate::db::sqlite::session_start_runtime_tx(
                tx,
                WorkerSessionInit {
                    id: "proxy-session".into(),
                    card_id: card.id.to_string(),
                    kind: WorkerSessionKind::ManagedAssistant,
                    agent_provider: Some(AgentProvider::Claude),
                    status: WorkerSessionState::Idle,
                    terminal_run_id: None,
                    thread_id: None,
                    session_id: None,
                    active_turn_id: None,
                    handle_state_json: None,
                    spawn_op_id: None,
                    now_ms: crate::model::now_ms(),
                },
            )
            .await?;
            Ok(card.id.to_string())
        })
    })
    .await
    .unwrap();
    let wiring = PlannerWiring::unconfigured_for_test(repo.clone());
    for configured in [false, true] {
        let (http, https) = if configured {
            repo.settings_upsert("http_proxy", "http://configured-model-proxy:8281")
                .await
                .unwrap();
            repo.settings_upsert("https_proxy", "http://configured-model-proxy:8282")
                .await
                .unwrap();
            (
                "http://configured-model-proxy:8281",
                "http://configured-model-proxy:8282",
            )
        } else {
            (
                "http://ambient-model-proxy:8181",
                "http://ambient-model-proxy:8182",
            )
        };
        for unset in [false, true] {
            let proxies = if unset {
                "unset HTTP_PROXY http_proxy HTTPS_PROXY https_proxy"
            } else {
                concat!(
                    "export HTTP_PROXY=http://project-proxy:8381 ",
                    "http_proxy=http://project-proxy:8381 ",
                    "HTTPS_PROXY=http://project-proxy:8382 ",
                    "https_proxy=http://project-proxy:8382"
                )
            };
            std::fs::write(
                root.path().join(".envrc"),
                format!("export GH_TOKEN=synthetic-project-tool-token\n{proxies}\n"),
            )
            .unwrap();
            assert!(
                Command::new("direnv")
                    .arg("allow")
                    .arg(root.path())
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
            let session = crate::claude_planner::wiring::open_session(
                &wiring.claude,
                &wiring,
                repo.clone(),
                Arc::new(ThreadSeals::new()),
                PlannerRow {
                    binding: PlannerBinding {
                        profile: HarnessProfile::Assistant,
                        provider: AgentProvider::Claude,
                    },
                    worker_session_id: "proxy-session",
                    card_id: &card_id,
                    track_id: track.id.as_str(),
                    prior_total_tokens: 0,
                },
            )
            .await
            .unwrap();
            let params = &session.shared.params;
            let logs = root.path().join(format!("launch-{configured}-{unset}"));
            std::fs::create_dir(&logs).unwrap();
            let env = spawn::session_env(params, Path::new("/operator-config")).unwrap();
            let script = r#"test "$HTTP_PROXY" = "$1" && test "$http_proxy" = "$1" &&
test "$HTTPS_PROXY" = "$2" && test "$https_proxy" = "$2" &&
printf '%s' "$GH_TOKEN" > child-tool-token"#;
            let (intent, payload) = crate::claude_planner::execution::intent(
                params,
                &ExecutionId::fresh().unwrap(),
                &logs,
                Path::new("/bin/sh"),
                vec![
                    "-c".into(),
                    script.into(),
                    "observe".into(),
                    http.into(),
                    https.into(),
                ],
                env,
                Some("synthetic-kernel-token".into()),
                &historical_admission(),
            )
            .await
            .unwrap();
            let serialized = serde_json::to_string(&intent.metadata).unwrap();
            assert!(!serialized.contains("synthetic-project-tool-token"));
            assert!(!serialized.contains("synthetic-kernel-token"));
            let mut child = Command::new(&intent.metadata.program.program)
                .args(&intent.metadata.program.args)
                .env_clear()
                .envs(intent.metadata.program.environment)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            child.stdin.take().unwrap().write_all(&payload).unwrap();
            assert!(child.wait_with_output().unwrap().status.success());
            assert_eq!(
                std::fs::read_to_string(root.path().join("child-tool-token")).unwrap(),
                "synthetic-project-tool-token"
            );
            std::fs::remove_file(root.path().join("child-tool-token")).unwrap();
        }
    }
}
