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
mod restart;
mod schema;

async fn fixture() -> SqlxRepo {
    fixture_at("sqlite::memory:").await
}

async fn fixture_at(url: &str) -> SqlxRepo {
    let repo = SqlxRepo::open(url).await.unwrap();
    for sql in [
        "INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('a','a','',0,0,0)",
        "INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('t','a','t',0,0,0),('other','a','other',1,0,0)",
        "INSERT INTO cards(id,track_id,kind,role,sort,created_at,updated_at) VALUES('p','t','planner',
        'planner',0,0,0),('a','t','codex','assistant',1,0,0),('w','t','codex','worker',2,0,0),('foreign',
        'other','planner','planner',0,0,0)",
        "INSERT INTO worker_sessions(id,track_id,provider,mode,contract,state,card_id,created_at_ms,
        updated_at_ms) VALUES('p','t','codex','resumable','planner','idle','p',0,0),('a','t','codex',
        'resumable','assistant','idle','a',0,0),('w','t','codex','ephemeral','executor','idle','w',0,0),
        ('foreign','other','codex','resumable','planner','idle','foreign',0,0)",
        "UPDATE cards SET session_id=id",
        "UPDATE tracks SET root_session_id=CASE id WHEN 't' THEN 'p' ELSE 'foreign' END",
    ] {
        sqlx::query(sql).execute(repo.pool()).await.unwrap();
    }
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
