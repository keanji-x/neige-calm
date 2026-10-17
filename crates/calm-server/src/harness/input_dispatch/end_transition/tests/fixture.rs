//! Seed real owner rows/authentication; all input/dispatch/END behavior goes
//! through the production facade, provider and canonical transaction owners.
use super::super::*;
use crate::auth::{AuthConfig, AuthState, HumanInputSource, SessionAuthority, SessionIdentity};
use crate::db::prelude::*;
use crate::db::sqlite::{SqlxRepo, begin_immediate_tx};
use crate::harness::backend::InputDelivery;
use crate::harness::held_requests::HeldRequestReceiver;
use crate::harness::planner_event::PlannerEventKind;
use crate::harness::profile::{HarnessProfile, PlannerBinding};
use crate::harness::wiring::{PlannerRow, PlannerWiring};
use crate::model::{CardRole, NewArea, NewCard, NewTrack, RequestTheme, new_id};
use crate::planner_model::TurnModelSelection;
use crate::planner_permission_mode::PlannerPermissionMode;
use crate::session_projection_repo::{WorkerSessionInit, WorkerSessionState};
use crate::shared_codex_appserver::SharedCodexAppServer;
use crate::thread_seals::ThreadSeals;
use calm_types::conversation::*;
use calm_types::organization::LEGACY_OWNER_USER_ID;
use std::sync::Arc;
use std::time::Duration;

pub(super) const HUMAN_BODY: &str = "private original human input";
pub(super) const CONTEXT_BODY: &str = "private frozen turn context";

pub(super) struct Fixture {
    pub repo: Arc<SqlxRepo>,
    pub backend: PlannerBackend,
    pub daemon: Arc<SharedCodexAppServer>,
    pub card: String,
    pub runtime: String,
    pub source: HumanInputSource,
    pub auth: AuthState,
    pub session: String,
    pub submission: InputSubmissionId,
    pub message: MessageRevisionRef,
    pub dir: tempfile::TempDir,
    _held: HeldRequestReceiver,
}

impl Fixture {
    pub async fn new(provider: AgentProvider) -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("data")).unwrap();
        let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
        let auth = AuthState::new(AuthConfig {
            username: Some("Owner".into()),
            password: Some("fixture-password".into()),
            dev_autologin: false,
            display_name: "Owner".into(),
        });
        let session = auth.sessions.create(
            SessionIdentity::owner(&auth.config),
            SessionAuthority::PasswordLogin,
        );
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            format!("{}={session}", crate::auth::SESSION_COOKIE)
                .parse()
                .unwrap(),
        );
        let (status, source) = crate::auth::authenticated_source(auth.clone(), headers).await;
        assert_eq!(status, axum::http::StatusCode::OK);
        let source = source.unwrap();
        let area = repo
            .area_create(
                LEGACY_OWNER_USER_ID,
                NewArea {
                    name: "canonical END".into(),
                    color: "#123456".into(),
                    sort: None,
                },
            )
            .await
            .unwrap();
        let track = repo
            .track_create(NewTrack {
                area_id: area.id,
                title: "canonical END".into(),
                sort: None,
                cwd: dir.path().to_string_lossy().into(),
                template_id: None,
                plugin_scope: None,
                template_input: None,
                attach_folder: false,
                theme: RequestTheme::default_dark(),
            })
            .await
            .unwrap();
        let mut tx = begin_immediate_tx(repo.pool()).await.unwrap();
        let card = crate::db::sqlite::card_create_with_id_tx(
            &mut tx,
            new_id(),
            NewCard {
                track_id: track.id.clone(),
                kind: "codex".into(),
                title: None,
                sort: None,
                payload: serde_json::json!({"schemaVersion":1,"planner_harness":true,
                    "planner_provider":provider,"permission_mode":"never"}),
            },
            CardRole::Planner,
            true,
            repo.card_role_cache(),
        )
        .await
        .unwrap()
        .id
        .to_string();
        let runtime = new_id();
        let thread = uuid::Uuid::new_v4().to_string();
        let mut frame = crate::harness::snapshot::CanonicalHarnessSnapshot::dormant(0);
        frame.header.last_thread_id = Some(thread.clone());
        crate::db::sqlite::session_start_runtime_tx(
            &mut tx,
            WorkerSessionInit::shared_planner(
                runtime.clone(),
                card.clone(),
                provider.clone(),
                WorkerSessionState::Idle,
                Some(thread.clone()),
                serde_json::to_value(frame).unwrap(),
                0,
            ),
        )
        .await
        .unwrap();
        let mut carrier = current_input_carrier_tx(&mut tx, &card, &runtime)
            .await
            .unwrap();
        let accepted = crate::conversation_inputs::accept_human_tx(
            &mut tx,
            &source.current_principal().unwrap(),
            &mut carrier,
            body(HUMAN_BODY),
        )
        .await
        .unwrap();
        let AcceptedInputRef::Human { message } = accepted.input_ref else {
            panic!("human")
        };
        let claimed = crate::conversation_inputs::claim_human_initial_tx(
            &mut tx,
            &source,
            &mut carrier,
            plan(message.clone()),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let mut wiring = PlannerWiring::unconfigured_for_test(repo.clone());
        match &provider {
            AgentProvider::Claude => {
                let bin = dir.path().join("claude");
                std::fs::copy(
                    concat!(
                        env!("CARGO_MANIFEST_DIR"),
                        "/tests/fixtures/claude_planner_fake/claude.sh"
                    ),
                    &bin,
                )
                .unwrap();
                std::fs::write(dir.path().join("scenario"), "exit").unwrap();
                wiring.claude = Arc::new(
                    crate::claude_planner::config::ClaudePlannerHost::new(
                        Some(crate::claude_planner::config::ClaudePlannerConfig {
                            claude_binary: bin,
                            claude_version: "2.1.280".into(),
                            config_dir: dir.path().join("config"),
                        }),
                        &dir.path().join("data"),
                        dir.path().join("unused-shim"),
                        dir.path().join("mcp.sock"),
                    )
                    .unwrap(),
                );
            }
            AgentProvider::OpenCode => {
                std::fs::write(dir.path().join("scenario"), "reply").unwrap();
                wiring.acp = Arc::new(
                    crate::acp_planner::config::AcpPlannerHost::new(
                        Some(crate::acp_planner::config::AcpPlannerConfig {
                            agents: vec![crate::acp_planner::config::AcpAgentConfig {
                                provider: provider.clone(),
                                command: "/usr/bin/python3".into(),
                                args: vec![
                                    "-u".into(),
                                    concat!(
                                        env!("CARGO_MANIFEST_DIR"),
                                        "/tests/fixtures/acp_planner/agent.py"
                                    )
                                    .into(),
                                ],
                                env: [(
                                    "ACP_FIXTURE_ROOT".into(),
                                    dir.path().to_string_lossy().into_owned(),
                                )]
                                .into(),
                                expected_agent_name: "Fixture ACP".into(),
                                expected_agent_version: "1".into(),
                            }],
                        }),
                        &dir.path().join("data"),
                        dir.path().join("unused-shim"),
                        dir.path().join("mcp.sock"),
                    )
                    .unwrap(),
                );
            }
            AgentProvider::Codex => {}
        }
        let daemon = if provider == AgentProvider::Codex {
            SharedCodexAppServer::new_stub(repo.clone())
        } else {
            SharedCodexAppServer::new_fake_running_with_pending(repo.clone(), None)
        };
        let backend = PlannerBackend::open(
            provider.clone(),
            daemon.clone(),
            Arc::new(ThreadSeals::new()),
            &wiring,
            repo.clone(),
            PlannerRow {
                binding: PlannerBinding {
                    profile: HarnessProfile::Planner,
                    provider,
                },
                worker_session_id: &runtime,
                card_id: &card,
                track_id: track.id.as_str(),
                prior_total_tokens: 0,
            },
        )
        .await
        .unwrap();
        let (held, receiver) = tokio::sync::mpsc::unbounded_channel();
        backend.mark_installed(&held);
        Self {
            repo,
            backend,
            daemon,
            card,
            runtime,
            source,
            auth,
            session,
            submission: claimed.submission_id,
            message,
            dir,
            _held: receiver,
        }
    }

    pub async fn dispatch(&self) -> crate::harness::input_dispatch::CommittedInputReport {
        let mut events = self.backend.subscribe_events();
        let report = crate::harness::input_dispatch::initial(
            self.repo.as_ref(),
            &self.source,
            &self.card,
            &self.runtime,
            &self.submission,
            &self.backend,
            &TurnModelSelection::inherit(),
            PlannerPermissionMode::Never,
            None,
        )
        .await
        .unwrap();
        assert!(matches!(
            report.report.delivery(),
            InputDelivery::Unknown(_)
        ));
        assert!(report.report.outcome().is_ok());
        tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                if matches!(
                    events.recv().await.unwrap().kind,
                    PlannerEventKind::TurnCompleted { .. }
                ) {
                    break;
                }
            }
        })
        .await
        .unwrap();
        report
    }

    pub async fn sample(&self) -> Sample {
        let mut tx = begin_immediate_tx(self.repo.pool()).await.unwrap();
        let sampled = sample_tx(&mut tx, &self.card, &self.runtime, &self.backend.provider())
            .await
            .unwrap()
            .unwrap();
        tx.commit().await.unwrap();
        sampled
    }

    pub async fn observe(&self, sampled: &Sample) -> QualifiedExecutionEnd {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Some(end) = self
                    .backend
                    .qualify_execution_end(&sampled.target)
                    .await
                    .unwrap()
                {
                    return end;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }

    pub async fn checkpoint(&self) -> ConversationInputCheckpoint {
        let mut tx = begin_immediate_tx(self.repo.pool()).await.unwrap();
        let carrier = current_input_carrier_tx(&mut tx, &self.card, &self.runtime)
            .await
            .unwrap();
        let value = carrier.checkpoint().clone();
        tx.commit().await.unwrap();
        value
    }

    pub async fn stop(&self) {
        self.backend.hibernate().await.unwrap();
    }
}

pub(super) fn body(text: &str) -> HumanPayload {
    HumanPayload {
        text: text.into(),
        bound_attachments: vec![],
    }
}

pub(super) fn plan(message: MessageRevisionRef) -> SubmissionRenderPlan {
    SubmissionRenderPlan::V1 {
        segments: vec![
            SubmissionRenderSegment::Input {
                input_position: 0,
                input: SubmissionRenderInput::Human {
                    message,
                    author: FrozenHumanAttribution {
                        registered_user_id: LEGACY_OWNER_USER_ID.into(),
                        display_name: "Owner".into(),
                    },
                },
            },
            SubmissionRenderSegment::Context {
                payload: PrivateHistoryPayload::Present {
                    value: SystemRenderContext::new(
                        calm_types::model::HarnessInputPresentation::System,
                        CONTEXT_BODY.into(),
                    )
                    .unwrap(),
                },
            },
        ],
    }
}
