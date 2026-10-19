//! A fresh real producer restores only the original unattempted System input.
use super::*;
use crate::db::sqlite::conversation::{self, StoredSubmissionIdentity, StoredSubmissionPlan};
use calm_types::conversation::InputSubmissionId;

const LATER_ENRICHMENT: &str = "A later report revision must not replace the frozen original input";

async fn original(
    flow: &Flow,
    id: &InputSubmissionId,
) -> (StoredSubmissionIdentity, StoredSubmissionPlan) {
    let mut tx = crate::db::sqlite::begin_immediate_tx(flow.fixture.repo.db())
        .await
        .unwrap();
    let identity = conversation::submission_identity_tx(&mut tx, id)
        .await
        .unwrap();
    let plan = conversation::submission_plan_tx(&mut tx, id).await.unwrap();
    tx.rollback().await.unwrap();
    (identity, plan)
}

enum ColdInput {
    FrozenEnrichment,
    ChangedOriginalEvent,
    HumanTail,
    RevokedHumanTail,
}

async fn cold_system_claim(case: ColdInput) {
    let change_original_row = matches!(case, ColdInput::ChangedOriginalEvent);
    let cold_human_tail = matches!(case, ColdInput::HumanTail | ColdInput::RevokedHumanTail);
    let revoke_human = matches!(case, ColdInput::RevokedHumanTail);
    let mut flow = Flow::running().await;
    flow.end(TURN).await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    // Only reject a new crossing marker. The actual producer and scheduler must
    // accept and claim the System input; the fixture never writes lifecycle state.
    sqlx::query(&format!("CREATE TRIGGER hold_system_marker BEFORE UPDATE OF handle_state_json ON worker_sessions
        WHEN NEW.id='{}' AND json_array_length(NEW.handle_state_json,'$.canonical_inputs.attempts')>1
        BEGIN SELECT RAISE(ABORT,'hold original System crossing marker'); END", flow.worker))
        .execute(flow.fixture.repo.pool()).await.unwrap();
    let event = flow.report(SYSTEM_BODY).await;
    let claimed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let frame = flow.fixture.carrier(&flow.track).await.2;
            if frame.header.phase == HarnessPhaseTag::IssuingTurn
                && frame.canonical_inputs.active.is_some() && frame.canonical_inputs.attempts.len()==1 {
                break frame;
            }
            tokio::select! {
                call = request(&mut flow.peer) => {
                    assert_eq!(call["method"], "config/read", "the crossing marker is held: {call}");
                    send(&mut flow.peer, json!({"id":call["id"],"result":{"config":{}}})).await;
                }
                _ = tokio::time::sleep(Duration::from_millis(5)) => {}
            }
        }
    }).await.expect("the original System claim must become durable without a native attempt");
    let active = claimed.canonical_inputs.active.clone().unwrap();
    let id = active.submission_ids[0].clone();
    let immutable = original(&flow, &id).await;
    let receipt = flow.fixture.receipt(&flow.area).await;
    if !change_original_row && !cold_human_tail {
        flow.report(LATER_ENRICHMENT).await;
    }
    assert_eq!(immutable.1.plan.input_refs().unwrap().len(), 1);
    flow.close().await;
    let shutdown_checkpoint = flow.fixture.carrier(&flow.track).await.2.canonical_inputs;
    assert_eq!(shutdown_checkpoint.active, claimed.canonical_inputs.active);
    assert_eq!(
        shutdown_checkpoint.attempts,
        claimed.canonical_inputs.attempts
    );
    assert_eq!(
        shutdown_checkpoint.pending.len(),
        usize::from(!change_original_row && !cold_human_tail)
    );
    if change_original_row {
        // A real persisted event keeps its ID and type but no longer has its
        // accepted original content. This is a source-integrity negative, not
        // a replacement observation or a manually advanced worker state.
        sqlx::query("UPDATE events SET payload=json_set(payload,'$.body_after','changed original event after acceptance') WHERE id=?1")
            .bind(event.id).execute(flow.fixture.repo.pool()).await.unwrap();
    }
    let Flow {
        mut fixture,
        state,
        app,
        cookie: old_cookie,
        area,
        track,
        card,
        worker,
        _client,
        peer,
    } = flow;
    drop((state, app, _client, peer));
    fixture.repo.pool().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, app, cookie) = fixture.application().await;
    assert_ne!(cookie, old_cookie);
    let mut accepted_human = None;
    if cold_human_tail {
        // The actual boot has no native connection or live handle. This POST
        // must retain only H2's own source even though it cannot restore S1.
        assert!(state.harness.get(&worker).is_none());
        let accepted = human(&app, &cookie, &card).await;
        assert_eq!(accepted["worker_session_id"], worker);
        let checkpoint = fixture.carrier(&track).await.2.canonical_inputs;
        assert_eq!(checkpoint.active, shutdown_checkpoint.active);
        assert_eq!(checkpoint.attempts, shutdown_checkpoint.attempts);
        assert!(matches!(
            checkpoint.pending.as_slice(),
            [PendingInput::Human { .. }]
        ));
        accepted_human = Some(checkpoint.pending[0].clone());
        if revoke_human {
            post(
                &app,
                &cookie,
                "/api/auth/logout",
                "revoke-cold-tail",
                json!({}),
            )
            .await;
            let whoami = app
                .clone()
                .oneshot(
                    Request::get("/api/auth/whoami")
                        .header(header::COOKIE, &cookie)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(
                whoami.status(),
                StatusCode::UNAUTHORIZED,
                "the actual accepting Human session was revoked before native recovery"
            );
        }
    }
    let (client, peer) = connect(&state.shared_codex_appserver).await;
    let mut flow = Flow {
        fixture,
        state,
        app,
        cookie,
        area,
        track,
        card,
        worker,
        _client: client,
        peer,
    };
    sqlx::query("DROP TRIGGER hold_system_marker")
        .execute(flow.fixture.repo.pool())
        .await
        .unwrap();
    // The original production reconcile reads current stored rows and re-admits
    // at/below the accepted watermark. No test passes a source or observation.
    flow.state.dispatcher.reconcile_tick_for_test().await;
    let mut reads = 0;
    let mut resumes = 0;
    let resumed = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let call = request(&mut flow.peer).await;
            let result = match call["method"].as_str().unwrap() {
                "config/read" => json!({"config":{}}),
                "thread/read" => {
                    reads += 1;
                    assert_eq!(call["params"]["threadId"], THREAD);
                    json!({"thread":{"id":THREAD,"status":{"type":"notLoaded"},"turns":[]}})
                }
                "thread/resume" => {
                    resumes += 1;
                    assert_eq!(call["params"]["threadId"], THREAD);
                    json!({"thread":{"id":THREAD,"status":{"type":"idle"},"turns":[]}})
                }
                "turn/start" => break call,
                method => {
                    panic!("cold restoration must preserve the same original thread: {method}")
                }
            };
            send(&mut flow.peer, json!({"id":call["id"],"result":result})).await;
        }
    })
    .await
    .ok();
    let resumed_input = resumed
        .as_ref()
        .map(|call| call["params"]["input"].to_string());
    let crossing = flow.fixture.carrier(&flow.track).await.2;
    if let Some(call) = &resumed {
        flow.accept(call, "cold-system-turn").await;
        flow.end("cold-system-turn").await;
        flow.fixture
            .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
            .await;
    }
    let mut later_continued = false;
    if !change_original_row
        && resumed.is_some()
        && let Some(call) = flow.next().await
    {
        assert!(
            call["params"]["input"]
                .to_string()
                .contains(if cold_human_tail {
                    HUMAN_BODY
                } else {
                    LATER_ENRICHMENT
                })
        );
        flow.accept(&call, "cold-later-report-turn").await;
        flow.end("cold-later-report-turn").await;
        flow.fixture
            .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
            .await;
        later_continued = true;
    }
    let after_immutable = original(&flow, &id).await;
    let after_receipt = flow.fixture.receipt(&flow.area).await;
    let carrier = flow.fixture.carrier(&flow.track).await;
    let counts = flow.fixture.counts(&flow.card).await;
    if flow.state.harness.get(&flow.worker).is_some() {
        flow.close().await;
    } else {
        flow.state.dispatcher.retire();
        flow.peer.close(None).await.unwrap();
    }
    assert_eq!(
        after_immutable, immutable,
        "re-admission never remints the plan/submission/Turn"
    );
    assert_eq!(after_receipt, receipt);
    assert_eq!((&carrier.0, &carrier.1), (&flow.card, &flow.worker));
    assert_eq!(carrier.2.header.last_thread_id.as_deref(), Some(THREAD));
    assert_eq!(
        (counts.0, counts.1, counts.2, counts.4),
        (
            1 + i64::from(cold_human_tail),
            1 + i64::from(cold_human_tail),
            1,
            0
        )
    );
    if change_original_row {
        assert!(
            resumed.is_none(),
            "a changed same-ID event cannot back the old accepted System input"
        );
        assert_eq!(
            (reads, resumes),
            (0, 0),
            "invalid source facts cannot spend even native restoration effects"
        );
        assert_eq!(carrier.2.canonical_inputs, shutdown_checkpoint);
    } else {
        let input = resumed_input
            .expect("a fresh real producer must restore its original unattempted System work");
        assert!(input.contains(SYSTEM_BODY));
        assert!(!input.contains(FIRST_BODY));
        assert!(
            !input.contains(if cold_human_tail {
                HUMAN_BODY
            } else {
                LATER_ENRICHMENT
            }),
            "later current enrichment cannot replace frozen content"
        );
        if revoke_human {
            assert!(
                !later_continued,
                "a revoked Human source must not inherit the recovered System producer's authority"
            );
            assert_eq!(
                carrier.2.canonical_inputs.pending,
                vec![accepted_human.unwrap()],
                "logout keeps the original Human reference, revision, position and acceptance time saved"
            );
            assert!(carrier.2.canonical_inputs.active.is_none());
        } else {
            assert!(
                later_continued,
                "the already accepted tail source must continue without another POST or event"
            );
        }
        assert_eq!((reads, resumes), (1, 1));
        assert_eq!(crossing.canonical_inputs.active.as_ref(), Some(&active));
        assert_eq!(crossing.canonical_inputs.attempts.len(), 2);
    }
}

#[tokio::test]
async fn fresh_production_boot_re_admits_the_original_system_claim_and_frozen_input() {
    cold_system_claim(ColdInput::FrozenEnrichment).await;
}
#[tokio::test]
async fn changed_same_id_event_cannot_re_admit_the_original_system_claim_after_reopen() {
    cold_system_claim(ColdInput::ChangedOriginalEvent).await;
}

#[tokio::test]
async fn human_tail_accepted_while_cold_system_native_is_down_continues_after_recovery() {
    cold_system_claim(ColdInput::HumanTail).await;
}

#[tokio::test]
async fn revoked_human_tail_stays_pending_when_cold_system_recovers_and_ends() {
    cold_system_claim(ColdInput::RevokedHumanTail).await;
}
