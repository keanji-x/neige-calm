use super::*;
use crate::db::sqlite::{SqlxRepo, area_delete_tx, track_delete_tx};
use crate::worker::Principal;
use objective::check::{CheckConfig, CommandOutput, CommandParams, MetricOutput, SampleOutput};
use objective::definition::{
    Budget, CheckDefinition, CompactPolicy, Mode, ObjectiveDefinition, Role, Usage,
};
use objective::ids::{CheckId, EvaluationId, ObjectiveKey, Revision};
use objective::ledger::Decision;
use objective::standard::{Approval, standard_hash};

mod lifecycle;
mod refusal;
mod replay;
mod report;
mod restart;
mod schema;
mod usage;

async fn fixture() -> SqlxRepo {
    fixture_at("sqlite::memory:").await
}

async fn fixture_at(url: &str) -> SqlxRepo {
    fixture_with_capability(url, true).await
}

async fn fixture_with_capability(url: &str, enabled: bool) -> SqlxRepo {
    let repo = SqlxRepo::open(url).await.unwrap();
    for sql in [
        "INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('a','a','',0,0,0)",
        "INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('t','a','t',0,0,0),('other','a','other',1,0,0)",
    ] {
        sqlx::query(sql).execute(repo.pool()).await.unwrap();
    }
    if enabled {
        let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
        for track in ["t", "other"] {
            crate::db::sqlite::track_capabilities::stamp_tx(
                &mut tx,
                &track.into(),
                &[calm_types::track_capability::TrackCapability::Objectives],
            )
            .await
            .unwrap();
        }
        tx.commit().await.unwrap();
    }
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let cache = crate::card_role_cache::CardRoleCache::new();
    for (id, track, role, payload) in [
        (
            "p",
            "t",
            crate::model::CardRole::Planner,
            serde_json::json!({"planner_provider":"codex"}),
        ),
        (
            "a",
            "t",
            crate::model::CardRole::Assistant,
            serde_json::json!({"harness_profile":"assistant","assistant_provider":"codex"}),
        ),
        (
            "w",
            "t",
            crate::model::CardRole::Worker,
            serde_json::json!({}),
        ),
        (
            "foreign",
            "other",
            crate::model::CardRole::Planner,
            serde_json::json!({"planner_provider":"codex"}),
        ),
    ] {
        crate::db::sqlite::card_create_with_id_tx(
            &mut tx,
            id.into(),
            crate::model::NewCard {
                track_id: track.into(),
                title: None,
                kind: "codex".into(),
                sort: None,
                payload,
            },
            role,
            false,
            &cache,
        )
        .await
        .unwrap();
        use crate::session_projection_repo::{AgentProvider, WorkerSessionInit, WorkerSessionKind};
        use calm_types::worker::WorkerSessionState;
        let init = match role {
            crate::model::CardRole::Planner => WorkerSessionInit::shared_planner(
                id.into(),
                id.into(),
                AgentProvider::Codex,
                WorkerSessionState::Idle,
                None,
                serde_json::json!({}),
                0,
            ),
            crate::model::CardRole::Assistant => WorkerSessionInit::managed_assistant(
                id.into(),
                id.into(),
                AgentProvider::Codex,
                WorkerSessionState::Idle,
                None,
                serde_json::json!({}),
                0,
            ),
            _ => WorkerSessionInit {
                id: id.into(),
                card_id: id.into(),
                kind: WorkerSessionKind::CodexCard,
                agent_provider: None,
                status: WorkerSessionState::Idle,
                terminal_run_id: None,
                thread_id: None,
                session_id: None,
                active_turn_id: None,
                handle_state_json: None,
                spawn_op_id: None,
                now_ms: 0,
            },
        };
        crate::db::sqlite::session_start_runtime_tx(&mut tx, init)
            .await
            .unwrap();
    }
    tx.commit().await.unwrap();
    repo
}

fn definition() -> ObjectiveDefinition {
    ObjectiveDefinition {
        key: ObjectiveKey::new("build"),
        title: "Build".into(),
        mode: Mode::Loop { plateau: 2 },
        checks: vec![CheckDefinition {
            id: CheckId::new("check"),
            role: Role::Ratchet,
            config: CheckConfig::Command(CommandParams {
                command: "true".into(),
            }),
        }],
        constraints: vec![],
        budget: Budget {
            turns: 10,
            tokens: 50000,
            wall_secs: 3600,
        },
        compact: CompactPolicy::EverySettle,
    }
}
fn planner(event: Event, id: &str) -> ObjectiveInput {
    ObjectiveInput {
        track_id: TrackId::from("t"),
        event,
        source: InputSource::Planner {
            principal: principal("p"),
            request_id: PlannerRequestId::new(id),
        },
    }
}
fn principal(session: &str) -> Principal {
    Principal::Agent {
        session_id: session.into(),
        track_id: "t".into(),
        area_id: "a".into(),
    }
}
fn harness(event: Event, turn: &str) -> ObjectiveInput {
    ObjectiveInput {
        track_id: "t".into(),
        event,
        source: InputSource::HarnessTurn {
            session_id: "p".into(),
            turn_id: turn.into(),
        },
    }
}
async fn counts(repo: &SqlxRepo) -> (i64, i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT count(*) FROM objective_events),(SELECT count(*) FROM objective_states),
        (SELECT count(*) FROM objective_intents)",
    )
    .fetch_one(repo.pool())
    .await
    .unwrap()
}
async fn fixture_react(repo: &SqlxRepo, event: Event, id: &str) -> Reaction {
    let input = if matches!(event, Event::HostIdle { .. } | Event::Usage { .. }) {
        harness(event, id)
    } else {
        planner(event, id)
    };
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    let result = react_fixture_tx(&mut tx, &input, 0).await.unwrap();
    tx.commit().await.unwrap();
    result
}
fn scenario() -> Vec<Event> {
    vec![
        Event::DefinitionChanged {
            objective: definition(),
        },
        Event::ApprovalGranted {
            objective: definition().key,
            approval: Approval {
                standard_hash: standard_hash(&definition()),
                approver: "human".into(),
            },
        },
        Event::Requested {
            objective: definition().key,
            revision: Revision::new("r0"),
        },
        Event::CheckFinished {
            objective: definition().key,
            evaluation: EvaluationId::new(0),
            check: CheckId::new("check"),
            revision: Revision::new("r0"),
            sample: 0,
            output: SampleOutput::Command(CommandOutput { exit_code: 0 }),
        },
        Event::Settled {
            objective: definition().key,
            evaluation: EvaluationId::new(0),
            decision: Decision::Keep,
            note: "baseline".into(),
        },
        Event::Usage {
            usage: Usage {
                turns: 2,
                context_tokens: 100,
                elapsed_secs: 10,
            },
        },
        Event::HostIdle { queue_len: 0 },
        Event::DefinitionChanged {
            objective: definition(),
        },
        Event::Closed {
            objective: definition().key,
        },
    ]
}

// Tests of storage semantics seed engine events explicitly without the production authorization door.
async fn fixture_input_react(repo: &SqlxRepo, input: ObjectiveInput) -> Result<CommittedReaction> {
    let mut tx = begin_immediate_tx(repo.db()).await?;
    let reaction = react_fixture_tx(&mut tx, &input, 0).await?;
    tx.commit().await?;
    Ok(CommittedReaction { reaction })
}
