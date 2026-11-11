//! Real current owners prepare the graph in a separate head database. Only the
//! fixed released column contracts below enter the historical migration input.
use super::*;
use crate::session_projection_repo::{
    AgentProvider, WorkerSessionInit, WorkerSessionProjectionRepo,
};

pub(super) enum Graph {
    Inline183,
    References184,
}

pub(super) async fn seed(repo: SqlxRepo, card: String, graph: Graph) -> Seed {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("head.db");
    let head = SqlxRepo::open(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .unwrap();
    super::super::tests::fixture_card_with_id(&head, AgentProvider::Codex, card.clone()).await;
    let current = seed_repo(head, card.clone()).await;
    if matches!(graph, Graph::References184) {
        current
            .repo
            .db()
            .writer()
            .with_writer(|session| {
                Box::pin(async {
                    let mut tx = session.begin_immediate().await.unwrap();
                    successor(&mut tx, &current).await;
                    tx.commit().await.unwrap();
                })
            })
            .await
            .unwrap();
    }
    let frame = current
        .repo
        .session_projection_active_for_card(&card)
        .await
        .unwrap()
        .unwrap();
    let (message, revision) = current
        .repo
        .db()
        .writer()
        .with_writer(|session| {
            Box::pin(async {
                let mut tx = session.begin_immediate().await.unwrap();
                let message = message_tx(&mut tx, &current.human.message_id)
                    .await
                    .unwrap();
                let revision = revision_tx(&mut tx, &current.human).await.unwrap();
                tx.commit().await.unwrap();
                (message, revision)
            })
        })
        .await
        .unwrap();
    let Seed {
        repo: head,
        carrier,
        root,
        human,
        render,
        ..
    } = current;
    drop(head);
    let write_db = repo.db().clone();
    write_db
        .writer()
        .with_writer(|session| {
            Box::pin(async move {
                session
                    .autocommit(|connection| {
                        Box::pin(async move {
                            sqlx::query("ATTACH DATABASE ?1 AS seed_head")
                                .bind(path.to_str().unwrap())
                                .execute(&mut **connection)
                                .await
                        })
                    })
                    .await
                    .unwrap();
                let mut tx = session.begin_immediate().await.unwrap();
                crate::db::sqlite::session_start_runtime_tx(
                    &mut tx,
                    WorkerSessionInit::shared_planner(
                        carrier.clone(),
                        card.clone(),
                        AgentProvider::Codex,
                        frame.status,
                        frame.thread_id,
                        frame.handle_state_json.unwrap(),
                        0,
                    ),
                )
                .await
                .unwrap();
                crate::db::sqlite::historical_fixtures::insert_message_before_guest_tx(
                    &mut tx, &message, &revision,
                )
                .await;
                // These projections are the 0175/0184 ABI, including its deferred cycles.
                // Adding a later column must not silently change a released fixture.
                for sql in [
                    "INSERT INTO conversation_turns(id,card_id,initiating_runtime_id,
             initial_submission_id,execution_kind,deployment_owner_user_id,provider,
             profile,created_at_ms)
             SELECT id,card_id,initiating_runtime_id,initial_submission_id,execution_kind,
             deployment_owner_user_id,provider,profile,created_at_ms
             FROM seed_head.conversation_turns",
                    "INSERT INTO conversation_input_submissions(id,card_id,logical_turn_id,ordinal,
             kind,render_plan_json,render_plan_sha256,native_correlation_json,created_at_ms)
             SELECT id,card_id,logical_turn_id,ordinal,kind,render_plan_json,
             render_plan_sha256,native_correlation_json,created_at_ms
             FROM seed_head.conversation_input_submissions WHERE render_plan_json IS NOT NULL",
                ] {
                    sqlx::query(sql).execute(&mut **tx).await.unwrap();
                }
                sqlx::query(
                    "INSERT INTO conversation_submission_inputs(submission_id,input_position,
            input_kind,message_id,revision,observation_id,queue_position,queued_at_ms,
            envelope_id,system_addressable,effective)
            SELECT submission_id,input_position,input_kind,message_id,revision,observation_id,
            queue_position,queued_at_ms,envelope_id,system_addressable,1
            FROM seed_head.conversation_submission_inputs WHERE submission_id=?1",
                )
                .bind(root.as_str())
                .execute(&mut **tx)
                .await
                .unwrap();
                if matches!(graph, Graph::References184) {
                    for sql in [
                "INSERT INTO conversation_input_retry_relations(retry_intent_id,card_id,
                 carrier_id,predecessor_submission_id,root_submission_id,
                 predecessor_attempt_id,resolution_key,principal_user_id,fingerprint,
                 created_at_ms,queue_position)
                 SELECT retry_intent_id,card_id,carrier_id,predecessor_submission_id,
                 root_submission_id,predecessor_attempt_id,resolution_key,principal_user_id,
                 fingerprint,created_at_ms,queue_position
                 FROM seed_head.conversation_input_retry_relations",
                "UPDATE conversation_submission_inputs SET effective=0,
                 retirement_reason='human_resolution',retired_by_retry_intent_id=(
                   SELECT retry_intent_id FROM conversation_input_retry_relations r
                   WHERE r.predecessor_submission_id=conversation_submission_inputs.submission_id)",
                "INSERT INTO conversation_input_submissions(id,card_id,logical_turn_id,ordinal,
                 kind,render_plan_json,render_plan_root_submission_id,render_plan_sha256,
                 native_correlation_json,retry_intent_id,created_at_ms)
                 SELECT id,card_id,logical_turn_id,ordinal,kind,render_plan_json,
                 render_plan_root_submission_id,render_plan_sha256,native_correlation_json,
                 retry_intent_id,created_at_ms FROM seed_head.conversation_input_submissions
                 WHERE render_plan_json IS NULL",
                "INSERT INTO conversation_submission_inputs(submission_id,input_position,
                 input_kind,message_id,revision,observation_id,queue_position,queued_at_ms,
                 envelope_id,system_addressable,effective)
                 SELECT submission_id,input_position,input_kind,message_id,revision,
                 observation_id,queue_position,queued_at_ms,envelope_id,system_addressable,
                 effective FROM seed_head.conversation_submission_inputs WHERE effective=1",
            ] {
                sqlx::query(sql).execute(&mut **tx).await.unwrap();
            }
                }
                sqlx::query(
                    "INSERT INTO conversation_presentations(sequence,card_id,source_kind,
            source_identity,message_id,observation_id,native_item_row_id,submission_id,
            erased_by_operation_id)
            SELECT sequence,card_id,source_kind,source_identity,message_id,observation_id,
            native_item_row_id,submission_id,erased_by_operation_id
            FROM seed_head.conversation_presentations",
                )
                .execute(&mut **tx)
                .await
                .unwrap();
                tx.commit().await.unwrap();
                session
                    .autocommit(|connection| {
                        Box::pin(async move {
                            sqlx::query("DETACH DATABASE seed_head")
                                .execute(&mut **connection)
                                .await
                        })
                    })
                    .await
                    .unwrap();
                Seed {
                    repo,
                    card,
                    carrier,
                    root,
                    human,
                    render,
                }
            })
        })
        .await
        .unwrap()
}
