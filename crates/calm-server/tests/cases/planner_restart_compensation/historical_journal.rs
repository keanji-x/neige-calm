//! Replay actual serialized historical output through the production compensation driver.
use super::*;

async fn replay(after_checkpoint: bool) {
    let mut boot = ordinary_track().await;
    let predecessor = boot.active().await;
    let verified_undo = Arc::new(std::sync::Mutex::new(None));
    let original = replace_start_adapter(
        &mut boot,
        StartFailure::HistoricalJournal {
            after_checkpoint,
            conflict: None,
            verified_undo: verified_undo.clone(),
        },
    )
    .await;
    let (status, body) = boot.fresh_start("restart").await;
    assert!(!status.is_success(), "injected failure: {body}");
    let (id, phase, output, state): (String, String, String, String) = sqlx::query_as(
        "SELECT id,phase,tx_output_json,compensation_state FROM operations \
         WHERE kind='planner-harness-start' AND json_extract(tx_output_json,?2)=?1 \
         ORDER BY created_at_ms DESC LIMIT 1",
    )
    .bind(&predecessor.id)
    .bind(format!("$.data.{PREDECESSOR_SESSION_KEY}"))
    .fetch_one(boot.repo.pool())
    .await
    .unwrap();
    let output: Value = serde_json::from_str(&output).unwrap();
    assert!(output["data"].get("old_session_cwd").is_none());
    if after_checkpoint {
        assert_eq!(phase, "stuck");
        assert_ne!(boot.current_session().await, Some(predecessor.id.clone()));
        // This snapshot has already been replaced with the successor: it is NOT undo proof.
        assert_eq!(
            output["result"]["payload"]["bound_session_cwd"]["worker_session_id"],
            output["data"][SUCCESSOR_SESSION_KEY]
        );
        let mut state: Value = serde_json::from_str(&state).unwrap();
        let restore = state["steps"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|step| step["op"] == "restore_old_runtime")
            .unwrap();
        assert_eq!(restore["completed"], false);
        // An operator supplies independently verified identity + cwd, never the successor cwd.
        restore["args"]["session_cwd"] = verified_undo.lock().unwrap().clone().unwrap();
        boot.repo.db().writer().execute(sqlx::query(
            "UPDATE operations SET compensation_state=?1,phase='compensating',phase_detail_json=NULL,completed_at_ms=NULL,\
             lease_owner=NULL,lease_until_ms=NULL WHERE id=?2 AND phase='stuck'",
        ).bind(serde_json::to_string(&state).unwrap()).bind(&id)).await.unwrap();
        let plan = original.operation_runtime.recover_on_boot().await.unwrap();
        original
            .operation_runtime
            .apply_recovery(plan)
            .await
            .unwrap();
        let phase: String = sqlx::query_scalar("SELECT phase FROM operations WHERE id=?1")
            .bind(&id)
            .fetch_one(boot.repo.pool())
            .await
            .unwrap();
        assert_eq!(
            phase, "failed",
            "verified repair finishes compensation on recovery"
        );
    } else {
        assert_eq!(
            phase, "failed",
            "a retained predecessor binding must replay automatically"
        );
    }
    assert_eq!(boot.current_session().await, Some(predecessor.id.clone()));
    let binding: String = sqlx::query_scalar(
        "SELECT json_extract(payload,'$.bound_session_cwd') FROM cards WHERE id=?1",
    )
    .bind(&boot.planner_card_id)
    .fetch_one(boot.repo.pool())
    .await
    .unwrap();
    let binding: Value = serde_json::from_str(&binding).unwrap();
    assert_eq!(
        binding,
        verified_undo.lock().unwrap().as_ref().unwrap()["binding"]
    );
    let restored = boot.active().await;
    assert_eq!(restored.thread_id, predecessor.thread_id);
    boot.state = original;
    boot.app = crate::planner_first_start::router(&boot.state).await;
    let (status, body) = post_input(
        boot.app.clone(),
        &boot.input_uri(),
        "after historical compensation",
        &new_id(),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["worker_session_id"], predecessor.id);
    boot.shutdown().await;
}

#[tokio::test]
async fn historical_journal_before_checkpoint_restores_from_retained_binding() {
    replay(false).await;
}

#[tokio::test]
async fn historical_journal_without_native_cwd_proof_requires_verified_repair_then_replays() {
    replay(true).await;
}

async fn refuses_conflicting_proof(conflict: HistoricalConflict) {
    let mut boot = ordinary_track().await;
    let predecessor = boot.active().await;
    let original = replace_start_adapter(
        &mut boot,
        StartFailure::HistoricalJournal {
            after_checkpoint: false,
            conflict: Some(conflict),
            verified_undo: Arc::new(std::sync::Mutex::new(None)),
        },
    )
    .await;
    let (status, body) = boot.fresh_start("restart").await;
    assert!(!status.is_success(), "injected failure: {body}");
    let phase: String = sqlx::query_scalar(
        "SELECT phase FROM operations WHERE kind='planner-harness-start' \
         AND json_extract(tx_output_json,?2)=?1 ORDER BY created_at_ms DESC LIMIT 1",
    )
    .bind(&predecessor.id)
    .bind(format!("$.data.{PREDECESSOR_SESSION_KEY}"))
    .fetch_one(boot.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        phase, "stuck",
        "conflicting historical facts cannot restore authority"
    );
    let state: String = sqlx::query_scalar("SELECT state FROM worker_sessions WHERE id=?1")
        .bind(&predecessor.id)
        .fetch_one(boot.repo.pool())
        .await
        .unwrap();
    assert_eq!(state, "superseded");
    boot.state = original;
    boot.shutdown().await;
}

#[tokio::test]
async fn historical_journal_refuses_a_changed_native_identity_in_the_saved_snapshot() {
    refuses_conflicting_proof(HistoricalConflict::NativeIdentity).await;
}

#[tokio::test]
async fn historical_journal_refuses_disagreeing_retained_cwds() {
    refuses_conflicting_proof(HistoricalConflict::Cwd).await;
}

#[tokio::test]
async fn an_explicit_null_thread_fact_survives_durable_compensation() {
    let mut boot = ordinary_track().await;
    let predecessor = boot.active().await;
    boot.repo.db().writer().execute(sqlx::query(
        "UPDATE worker_sessions SET handle_state_json=json_set(handle_state_json,'$.last_thread_id',NULL) WHERE id=?1",
    ).bind(&predecessor.id)).await.unwrap();
    let original = replace_start_adapter(&mut boot, StartFailure::BeforeNative).await;
    let (status, body) = boot.fresh_start("restart").await;
    assert!(!status.is_success(), "injected failure: {body}");
    assert_eq!(boot.current_session().await, Some(predecessor.id));
    boot.state = original;
    boot.shutdown().await;
}
