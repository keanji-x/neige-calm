use super::*;
use crate::db::RepoOutOfDomain;

async fn track(repo: &SqlxRepo, card: &str) -> String {
    sqlx::query_scalar("SELECT track_id FROM cards WHERE id=?1")
        .bind(card)
        .fetch_one(repo.pool())
        .await
        .unwrap()
}

#[tokio::test]
async fn u13_canonical_terminal_recorder_allows_reopen() {
    let (_dir, url) = database();
    let repo = SqlxRepo::open(&url).await.unwrap();
    let (card, worker) = carrier(&repo).await;
    repo.harness_turn_outcome_put(
        &worker,
        &card,
        &track(&repo, &card).await,
        "thread",
        "turn",
        r#"{"id":"turn","status":"completed"}"#,
    )
    .await
    .unwrap();
    let reopened = SqlxRepo::open(&url).await.expect("canonical terminal fact");
    reopened.db().close().await;
    sqlx::query("UPDATE harness_items SET method='unknown/future'")
        .execute(repo.pool())
        .await
        .unwrap();
    assert_boot_refused(&url).await;
    repo.db().close().await;
}

#[tokio::test]
async fn u13_plainchat_v1_receipt_allows_reopen() {
    let (_dir, url) = database();
    let repo = SqlxRepo::open(&url).await.unwrap();
    let (card, worker) = carrier_profile(&repo, true).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    planner_input_bind_tx(
        &mut tx,
        &card,
        "key",
        &PlannerInputBinding {
            payload_hash: "v1:plain".into(),
            worker_session_id: worker,
            entry_id: None,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let reopened = SqlxRepo::open(&url).await.expect("PlainChat receipt");
    reopened.db().close().await;
    // The same receipt without a proven session association must fail closed.
    sqlx::query("UPDATE planner_input_idempotency SET worker_session_id='missing'")
        .execute(repo.pool())
        .await
        .unwrap();
    assert_boot_refused(&url).await;
    repo.db().close().await;
}

#[tokio::test]
async fn u13_acp_historical_closed_retired_mixed_refuses_boot() {
    acp_format(r#"{"input":[],"prompt":[],"queue":[]}"#, false).await;
}
#[tokio::test]
async fn u13_acp_unknown_format_refuses_boot() {
    acp_format(r#"{"version":"unknown_future"}"#, false).await;
}
#[tokio::test]
async fn u13_acp_canonical_unknown_state_allows_boot() {
    let value = json!({"version":"canonical_v1","association":{
        "logical_turn_id":"44444444444444444444444444444444",
        "proof":{"version":"v2",
            "attempt":{"submission_id":"11111111111111111111111111111111","attempt_id":"22222222222222222222222222222222"},
            "input_refs":[{"kind":"system","observation_id":"33333333333333333333333333333333"}],
            "render_plan":{"version":"v1","submission_id":"11111111111111111111111111111111","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}}
    }});
    let canonical = AcpSubmissionInput::decode(&value.to_string())
        .unwrap()
        .encode()
        .unwrap();
    acp_format(&canonical, true).await;
}
async fn acp_format(raw: &str, canonical: bool) {
    let (_dir, url) = database();
    let repo = SqlxRepo::open(&url).await.unwrap();
    carrier(&repo).await;
    let (card, worker) = carrier(&repo).await;
    sqlx::query(
        "UPDATE cards SET payload=json_set(payload,'$.planner_provider','opencode') WHERE id=?1",
    )
    .bind(&card)
    .execute(repo.pool())
    .await
    .unwrap();
    sqlx::query("UPDATE worker_sessions SET provider='opencode',state='superseded' WHERE id=?1")
        .bind(&worker)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET closed_at=1 WHERE id=(SELECT track_id FROM cards WHERE id=?1)")
        .bind(&card)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO acp_managed_sessions VALUES(?1,'registration')")
        .bind(&worker)
        .execute(repo.pool())
        .await
        .unwrap();
    sqlx::query("INSERT INTO acp_submissions(worker_session_id,client_id,turn_id,thread_id,native_session_id,input_json,state,created_at_ms) VALUES(?1,'client','turn','thread','native',?2,'unknown',0)")
        .bind(&worker).bind(raw).execute(repo.pool()).await.unwrap();
    if canonical {
        let reopened = SqlxRepo::open(&url)
            .await
            .expect("canonical Unknown is a state, not a historical format");
        reopened.db().close().await;
        // Normal deletion cascades; this deliberately emulates damaged storage.
        sqlx::raw_sql("PRAGMA foreign_keys=OFF; UPDATE acp_managed_sessions SET worker_session_id='missing'; PRAGMA foreign_keys=ON;")
            .execute(repo.pool())
            .await
            .unwrap();
        assert_boot_refused(&url).await;
    } else {
        assert_boot_refused(&url).await;
    }
    repo.db().close().await;
}

#[tokio::test]
async fn u13_completed_retained_activation_survives_real_delete() {
    let (_dir, url) = database();
    let repo = SqlxRepo::open(&url).await.unwrap();
    let (card, worker) = carrier(&repo).await;
    let payload =
        json!({"canonical_activation_version":2,"card_id":card,"worker_session_id":worker});
    sqlx::query("INSERT INTO operations(id,operation_key,kind,payload_hash,target_type,target_json,payload_json,phase,created_at_ms,updated_at_ms) VALUES ('op','key','planner-harness-start','hash','card','{}',?1,'succeeded',0,0)")
        .bind(payload.to_string()).execute(repo.pool()).await.unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    card_delete_tx(&mut tx, &card, repo.card_role_cache())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let reopened = SqlxRepo::open(&url)
        .await
        .expect("completed canonical journal survives resource deletion");
    reopened.db().close().await;
    for phase in ["pending", "failed"] {
        sqlx::query("UPDATE operations SET phase=?1")
            .bind(phase)
            .execute(repo.pool())
            .await
            .unwrap();
        assert_boot_refused(&url).await;
    }
    sqlx::query("UPDATE operations SET phase='succeeded'")
        .execute(repo.pool())
        .await
        .unwrap();
    for bad in [
        json!({"canonical_activation_version":2,"card_id":"","worker_session_id":"worker"}),
        json!({"canonical_activation_version":2,"card_id":4,"worker_session_id":"worker"}),
        json!({"canonical_activation_version":2,"card_id":"card"}),
        json!({"canonical_activation_version":3,"card_id":"card","worker_session_id":"worker"}),
        json!({"canonical_activation_version":2,"card_id":"card","worker_session_id":"worker","extra":true}),
    ] {
        sqlx::query("UPDATE operations SET payload_json=?1")
            .bind(bad.to_string())
            .execute(repo.pool())
            .await
            .unwrap();
        assert_boot_refused(&url).await;
    }
    repo.db().close().await;
}

#[tokio::test]
async fn u13_creation_receipts_are_neutral_with_canonical_or_deleted_resources() {
    let (_dir, url) = database();
    let repo = SqlxRepo::open(&url).await.unwrap();
    let (card, _) = carrier(&repo).await;
    let track = track(&repo, &card).await;
    let area: String = sqlx::query_scalar("SELECT area_id FROM tracks WHERE id=?1")
        .bind(&track)
        .fetch_one(repo.pool())
        .await
        .unwrap();
    for (version, first) in [(2, None), (1, Some("a".repeat(64)))] {
        sqlx::query("INSERT OR REPLACE INTO track_create_idempotency(area_id,idempotency_key,track_id,planner_card_id,report_card_id,created_at_ms,request_fingerprint_version,create_request_sha256,first_message_sha256) VALUES (?1,'historical',?2,?3,'gone-report',0,?6,?4,?5)")
            .bind(&area).bind(&track).bind(&card).bind("b".repeat(64)).bind(first).bind(version)
            .execute(repo.pool()).await.unwrap();
        let reopened = SqlxRepo::open(&url)
            .await
            .expect("creation receipt has no format");
        reopened.db().close().await;
    }
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    card_delete_tx(&mut tx, &card, repo.card_role_cache())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let reopened = SqlxRepo::open(&url)
        .await
        .expect("deleted card and carriers leave no storage");
    reopened.db().close().await;
    repo.db().close().await;
}

#[tokio::test]
async fn u13_retained_discriminator_precedes_plainchat_exclusion() {
    let (_dir, url) = database();
    let repo = SqlxRepo::open(&url).await.unwrap();
    let (card, worker) = carrier(&repo).await;
    sqlx::query("INSERT INTO operations(id,operation_key,kind,payload_hash,target_type,target_json,payload_json,phase,created_at_ms,updated_at_ms) VALUES ('op','key','planner-harness-start','hash','card','{}','{}','succeeded',0,0)")
        .execute(repo.pool()).await.unwrap();
    let mut admitted = Vec::new();
    for deleted in [false, true] {
        if deleted {
            let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
            card_delete_tx(&mut tx, &card, repo.card_role_cache())
                .await
                .unwrap();
            tx.commit().await.unwrap();
        }
        for phase in ["succeeded", "pending", "failed"] {
            let plain = json!({"actor":"user","wave_id":"track","spec_card_id":card,
                "cwd":"/tmp","profile":"plain_chat"});
            sqlx::query("UPDATE operations SET phase=?1,payload_json=?2")
                .bind(phase)
                .bind(plain.to_string())
                .execute(repo.pool())
                .await
                .unwrap();
            let reopened = SqlxRepo::open(&url)
                .await
                .expect("untagged historical PlainChat");
            reopened.db().close().await;
            for payload in [
                json!({"canonical_activation_version":3,"card_id":card,"worker_session_id":worker,"profile":"plain_chat"}),
                json!({"canonical_activation_version":"future","card_id":card,"worker_session_id":worker,"profile":"plain_chat"}),
                json!({"canonical_activation_version":null,"card_id":card,"worker_session_id":worker,"profile":"plain_chat"}),
                json!({"canonical_activation_version":2,"card_id":card,"worker_session_id":worker,"profile":"plain_chat"}),
                json!({"canonical_activation_version":2,"card_id":card,"worker_session_id":worker,"profile":"plain_chat","extra":true}),
                json!({"canonical_activation_version":2,"card_id":4,"worker_session_id":worker,"profile":"plain_chat"}),
                json!({"canonical_activation_version":2,"card_id":card,"profile":"plain_chat"}),
                json!({"canonical_activation_version":2,"worker_session_id":worker,"profile":"plain_chat"}),
                json!({"canonical_activation_version":2,"card_id":"","worker_session_id":worker,"profile":"plain_chat"}),
            ] {
                sqlx::query("UPDATE operations SET payload_json=?1")
                    .bind(payload.to_string())
                    .execute(repo.pool())
                    .await
                    .unwrap();
                match SqlxRepo::open(&url).await {
                    Ok(reopened) => {
                        reopened.db().close().await;
                        admitted.push((deleted, phase, payload));
                    }
                    Err(error) => assert!(
                        error.to_string().contains("conversation upgrade"),
                        "{error}"
                    ),
                }
            }
        }
    }
    repo.db().close().await;
    assert!(
        admitted.is_empty(),
        "retained collisions admitted: {admitted:?}"
    );
}
