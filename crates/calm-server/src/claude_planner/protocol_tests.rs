//! Minimal role adapter through the real protocol loop, Engine and supervisor.
use super::driver::{TerminalEvent, decide};
use super::protocol_turn::{ProtocolTurn, drive};
use super::role::{ConfigSource, PlannerEnvironment, ProtocolProfile};
use super::spawn::{EnvInputs, base_env};
use super::turn_sink::TurnSink;
use super::turn_slot::{TerminalCause, TurnSlot};
use crate::db::prelude::*;
use crate::db::sqlite::SqlxRepo;
use crate::error::Result;
use crate::provider_execution::{self as execution, stdio::EngineStdio};
use provider::claude::protocol::{Record, SystemInit};
use provider::claude::translate::TurnOutcome;
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use uuid::Uuid;
use workspace_runtime::execution::test_support::intent;

#[derive(Default)]
struct Observed {
    init: bool,
    result: Option<TurnOutcome>,
    qualified: Option<Arc<AtomicBool>>,
    completed: bool,
}
struct MinimalOwner(Arc<Mutex<Observed>>);
#[async_trait::async_trait]
impl execution::ProviderExecutionOwner for MinimalOwner {
    fn consumer(&self) -> &'static str {
        "claude-planner"
    }
    fn alias_prefix(&self) -> execution::AliasPrefix {
        execution::AliasPrefix::planner()
    }
    fn alias(&self, session: &str, id: &str) -> String {
        self.alias_prefix().alias(session, id)
    }
    async fn complete(
        &self,
        _: &str,
        _: &execution::Record,
        evidence: execution::CompletionEvidence,
        known: Option<execution::KnownEnd>,
    ) -> Result<()> {
        assert!(matches!(
            evidence,
            execution::CompletionEvidence::Settled(_)
        ));
        assert!(known.is_none());
        let mut observed = self.0.lock().unwrap();
        observed.completed = true;
        observed
            .qualified
            .as_ref()
            .unwrap()
            .store(true, Ordering::Release);
        Ok(())
    }
}
struct MinimalProfile(Uuid);
impl ProtocolProfile for MinimalProfile {
    fn thread(&self) -> Uuid {
        self.0
    }
    fn init_check(&self, init: &SystemInit) -> std::result::Result<(), String> {
        if init.session_id == self.0 {
            Ok(())
        } else {
            Err("session".into())
        }
    }
}
struct MinimalSink {
    slot: Arc<TurnSlot>,
    pool: crate::db::sqlite::SqliteDb,
    engine: crate::execution_boot::ExecutionEngine,
    owner: execution::ProviderExecution,
    observed: Arc<Mutex<Observed>>,
}
#[async_trait::async_trait]
impl TurnSink for MinimalSink {
    fn slot(&self) -> Arc<TurnSlot> {
        self.slot.clone()
    }
    fn cancellation(&self, claim: &execution::Claim) -> execution::Cancellation {
        execution::Cancellation {
            owner: self.owner.clone(),
            pool: self.pool.clone(),
            engine: self.engine.clone(),
            session: "protocol-session".into(),
            claim: claim.clone(),
            armed: true,
        }
    }
    async fn observe_init(&mut self, _: &SystemInit) {
        self.observed.lock().unwrap().init = true;
    }
    async fn record(&mut self, _: &str, _: &Record, _: bool) {}
    fn result_event(&self, event: TerminalEvent) -> TerminalEvent {
        event
    }
    fn known_end(
        self,
        _: &execution::Claim,
        event: TerminalEvent,
        cause: Option<TerminalCause>,
        qualified: Arc<AtomicBool>,
    ) -> Option<execution::KnownEnd> {
        let mut observed = self.observed.lock().unwrap();
        observed.result = Some(decide(cause.as_ref(), &event));
        observed.qualified = Some(qualified);
        None
    }
}

async fn run_minimal(interrupt: bool) {
    use crate::model::{CardRole, NewArea, NewCard, NewTrack, RequestTheme, new_id};
    use crate::session_projection_repo::{
        AgentProvider, WorkerSessionInit, WorkerSessionKind, WorkerSessionState,
    };
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let pool = repo.sqlite_db().unwrap();
    let fixture = crate::test_support::execution::Fixture::new(pool.clone()).await;
    let dir = tempfile::tempdir().unwrap();
    let area = repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            NewArea {
                name: "protocol".into(),
                color: "#111111".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id,
            title: "protocol".into(),
            sort: None,
            cwd: dir.path().to_string_lossy().into(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let mut tx = pool.writer_pool().begin().await.unwrap();
    let card = crate::db::sqlite::card_create_with_id_tx(
        &mut tx,
        new_id(),
        NewCard {
            track_id: track.id,
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: serde_json::json!({}),
        },
        CardRole::Worker,
        false,
        &crate::card_role_cache::CardRoleCache::new(),
    )
    .await
    .unwrap();
    let thread = Uuid::new_v4();
    crate::db::sqlite::session_start_runtime_tx(
        &mut tx,
        WorkerSessionInit {
            id: "protocol-session".into(),
            card_id: card.id.to_string(),
            kind: WorkerSessionKind::ClaudeCard,
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
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let observed = Arc::new(Mutex::new(Observed::default()));
    let owner = execution::ProviderExecution {
        owner: Arc::new(MinimalOwner(observed.clone())),
        drivers: Default::default(),
    };
    let mut launch = intent(dir.path().into());
    launch.metadata.consumer = owner.owner.consumer().into();
    launch.metadata.alias.0 = owner
        .owner
        .alias("protocol-session", launch.metadata.id.as_str());
    launch.metadata.program.program = "/bin/sh".into();
    let init = serde_json::json!({"type":"system","subtype":"init","session_id":thread,
        "claude_code_version":"test","model":"test","capabilities":[],"mcp_servers":[],"slash_commands":[],"skills":[],"plugins":[]});
    let result = serde_json::json!({"type":"result","subtype":"success","session_id":thread,
        "is_error":false,"result":"done","usage":{"input_tokens":0,"cache_creation_input_tokens":0,"cache_read_input_tokens":0,"output_tokens":0,"iterations":[]},"modelUsage":{}});
    std::fs::write(dir.path().join("init.json"), format!("{init}\n")).unwrap();
    std::fs::write(dir.path().join("result.json"), format!("{result}\n")).unwrap();
    launch.metadata.program.args = vec![
        "-c".into(),
        if interrupt {
            "read start; cat init.json; exec sleep 120".into()
        } else {
            "read start; cat init.json result.json".into()
        },
    ];
    let id = launch.metadata.id.clone();
    let (claim, guard) = owner
        .admit_for_test(
            &pool,
            &fixture.engine,
            "protocol-session",
            execution::PreparedIntent(launch),
            execution::Purpose::RewindCheck,
        )
        .await
        .unwrap();
    execution::release::send(
        &pool,
        &fixture.engine,
        "protocol-session",
        &claim,
        b"start\n",
        &execution::release::NoAuthority,
    )
    .await
    .unwrap();
    let slot = Arc::new(TurnSlot::new(
        EngineStdio::new((*fixture.engine).clone(), id.clone()),
        Duration::from_secs(12),
    ));
    let sink = MinimalSink {
        slot: slot.clone(),
        pool: pool.clone(),
        engine: (*fixture.engine).clone(),
        owner: owner.clone(),
        observed: observed.clone(),
    };
    let drive = drive(
        sink,
        ProtocolTurn {
            claim,
            guard,
            stdout: EngineStdio::new((*fixture.engine).clone(), id.clone()),
            profile: MinimalProfile(thread),
        },
    );
    let stop = async {
        if interrupt {
            tokio::time::timeout(Duration::from_secs(10), async {
                while !observed.lock().unwrap().init {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .unwrap();
            slot.record(TerminalCause::Interrupted);
            slot.arm_stop(tokio::time::Instant::now());
        }
    };
    let (qualified, ()) =
        tokio::time::timeout(Duration::from_secs(20), async { tokio::join!(drive, stop) })
            .await
            .unwrap();
    assert!(qualified);
    {
        let observed = observed.lock().unwrap();
        assert!(observed.init && observed.completed);
        assert_eq!(
            observed.result,
            Some(if interrupt {
                TurnOutcome::Interrupted
            } else {
                TurnOutcome::Completed
            })
        );
    }
    assert!(slot.stdin.lock().await.is_none());
    assert!(!owner.drivers.contains(&id));
    assert!(
        calm_truth::db::sqlite::provider_execution::load(pool.read_pool(), "protocol-session")
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn w_protocol_minimal_sink_result() {
    run_minimal(false).await;
}
#[tokio::test]
async fn w_protocol_minimal_sink_interrupt() {
    run_minimal(true).await;
}

#[test]
fn w_protocol_user_home_omits_config_dir() {
    let env = base_env(
        &PlannerEnvironment,
        &EnvInputs {
            path: "/usr/bin".into(),
            config_source: ConfigSource::UserHome,
            mcp_socket: Path::new("/run/mcp.sock"),
            proxy: &[],
        },
    );
    assert!(!env.iter().any(|(key, _)| key == "CLAUDE_CONFIG_DIR"));
}
#[test]
fn w_protocol_dedicated_preserves_planner_env() {
    let env = base_env(
        &PlannerEnvironment,
        &EnvInputs {
            path: "/usr/bin".into(),
            config_source: ConfigSource::Dedicated(Path::new("/planner/config")),
            mcp_socket: Path::new("/run/mcp.sock"),
            proxy: &[],
        },
    );
    let mut expected: Vec<(String, std::ffi::OsString)> = super::spawn::passthrough_keys()
        .filter_map(|key| std::env::var_os(key).map(|value| (key.into(), value)))
        .collect();
    expected.extend([
        ("PATH".into(), "/usr/bin".into()),
        ("CLAUDE_CONFIG_DIR".into(), "/planner/config".into()),
        ("NEIGE_MCP_SOCKET".into(), "/run/mcp.sock".into()),
        ("DISABLE_AUTOUPDATER".into(), "1".into()),
        ("CLAUDE_CODE_DISABLE_AUTO_MEMORY".into(), "1".into()),
    ]);
    assert_eq!(env, expected);
}
