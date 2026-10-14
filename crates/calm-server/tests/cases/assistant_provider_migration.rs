//! The real pre-Assistant database is upgraded through the production migrator.
use crate::support::migration_replay::{
    assert_schema_matches_fresh, open_sqlite, replay_to_head, stage_db_at,
};
use serde_json::json;

async fn staged() -> (tempfile::TempDir, sqlx::SqlitePool) {
    let dir = tempfile::tempdir().unwrap();
    let pool = open_sqlite(&dir.path().join("old.db")).await;
    stage_db_at(&pool, 162).await;
    sqlx::query("INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('area','a','#000',0,1,1)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('track','area','t',0,1,1)").execute(&pool).await.unwrap();
    for (card, role, profile) in [
        ("assistant", "assistant", "assistant"),
        ("dormant", "assistant", "assistant"),
        ("planner", "planner", "unused-planner-marker"),
        ("worker", "worker", "plain_chat"),
    ] {
        sqlx::query(
            "INSERT INTO cards(id,track_id,kind,role,sort,payload,deletable,created_at, \
            updated_at) VALUES(?1,'track','codex',?2,0,?3,0,1,1)",
        )
        .bind(card)
        .bind(role)
        .bind(
            json!({"schemaVersion":1,"harness_profile":profile,"planner_provider":"codex"})
                .to_string(),
        )
        .execute(&pool)
        .await
        .unwrap();
    }
    for (id, card, contract, state) in [
        ("root", "planner", "planner", "idle"),
        ("old", "assistant", "executor", "superseded"),
        ("current", "assistant", "executor", "idle"),
        ("failed", "assistant", "executor", "failed"),
        ("worker-session", "worker", "executor", "idle"),
    ] {
        sqlx::query("INSERT INTO worker_sessions(id,track_id,card_id,provider,mode,contract,state,thread_id, \
            handle_state_json,created_at_ms,updated_at_ms,queue_harvested_at_ms) VALUES(?1,'track',?2, \
            'codex','resumable',?3,?4,?1,?5,1,2,123)")
            .bind(id).bind(card).bind(contract).bind(state).bind(json!({"pending_queue":["frozen"],"push_watermark":999}).to_string()).execute(&pool).await.unwrap();
    }
    sqlx::query("UPDATE cards SET session_id='current' WHERE id='assistant'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE cards SET session_id='root' WHERE id='planner'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET root_session_id='root'")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO operations(id,operation_key,kind,payload_hash,target_type,target_json,payload_json, \
        phase,created_at_ms,updated_at_ms) VALUES('op','legacy-key','planner-harness-start', \
        'frozen-hash','card','{}','{\"spec_card_id\":\"assistant\",\"profile\":\"assistant\"}', \
        'succeeded',1,1)").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO terminals(id,card_id,program,cwd,env,theme_fg,theme_bg,created_at) VALUES('term', \
        'worker','fake','/tmp','{}','255,255,255','0,0,0',1)").execute(&pool).await.unwrap();
    sqlx::query(
        "UPDATE worker_sessions SET parent_session_id='root',requester_session_id='current', \
        terminal_run_id='term',spawn_op_id='op' WHERE id='worker-session'",
    )
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query("INSERT INTO worker_flow_items(card_id,captured_session_id,track_id,worker_session_id,kind, \
        payload,created_at_ms) VALUES('worker','worker-session','track','worker-session','text','{}',1)").execute(&pool).await.unwrap();
    sqlx::query("UPDATE worker_sessions SET provider='opencode',agent_session_id='native-root' WHERE id='root'").execute(&pool).await.unwrap();
    sqlx::query("UPDATE cards SET payload=json_set(payload,'$.planner_provider','opencode') WHERE id='planner'").execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO acp_managed_sessions VALUES('root','registration')")
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO acp_submissions(worker_session_id,client_id,turn_id,thread_id,native_session_id, \
        input_json,state,created_at_ms) VALUES('root','client','turn','root','native-root', \
        '{\"queue\":[]}','unknown',1)").execute(&pool).await.unwrap();
    (dir, pool)
}

#[tokio::test]
async fn assistant_provider_migration_preserves_history_current_links_queue_and_schema() {
    let (_dir, pool) = staged().await;
    let before: Vec<(String,String,String,i64)> = sqlx::query_as("SELECT id,thread_id,handle_state_json,queue_harvested_at_ms FROM worker_sessions ORDER BY id").fetch_all(&pool).await.unwrap();
    let operation_before: (String, String) =
        sqlx::query_as("SELECT payload_json,payload_hash FROM operations WHERE id='op'")
            .fetch_one(&pool)
            .await
            .unwrap();
    replay_to_head(&pool).await;
    let after: Vec<(String,String,String,i64)> = sqlx::query_as("SELECT id,thread_id,handle_state_json,queue_harvested_at_ms FROM worker_sessions ORDER BY id").fetch_all(&pool).await.unwrap();
    assert_eq!(after, before);
    let contracts: Vec<String> =
        sqlx::query_scalar("SELECT contract FROM worker_sessions WHERE card_id='assistant'")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(contracts, vec!["assistant"; 3]);
    for card in ["assistant", "dormant"] {
        let provider: String = sqlx::query_scalar(
            "SELECT json_extract(payload,'$.assistant_provider') FROM cards WHERE id=?1",
        )
        .bind(card)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(provider, "codex");
    }
    let pointer: String = sqlx::query_scalar("SELECT session_id FROM cards WHERE id='assistant'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(pointer, "current");
    let root: String = sqlx::query_scalar("SELECT root_session_id FROM tracks WHERE id='track'")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(root, "root");
    let relations: (String, String, String, String) = sqlx::query_as(
        "SELECT parent_session_id,requester_session_id,terminal_run_id, \
        spawn_op_id FROM worker_sessions WHERE id='worker-session'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(
        relations,
        ("root".into(), "current".into(), "term".into(), "op".into())
    );
    let flow: String = sqlx::query_scalar("SELECT worker_session_id FROM worker_flow_items")
        .fetch_one(&pool)
        .await
        .unwrap();
    assert_eq!(flow, "worker-session");
    let journal:(String,String,String)=sqlx::query_as("SELECT state,native_session_id,input_json FROM acp_submissions WHERE worker_session_id='root'").fetch_one(&pool).await.unwrap();
    assert_eq!(
        journal,
        (
            "unknown".into(),
            "native-root".into(),
            "{\"queue\":[]}".into()
        )
    );
    let registration: String = sqlx::query_scalar(
        "SELECT registration_digest FROM acp_managed_sessions WHERE worker_session_id='root'",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(registration, "registration");
    let operation_after: (String, String) =
        sqlx::query_as("SELECT payload_json,payload_hash FROM operations WHERE id='op'")
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        operation_before, operation_after,
        "historical operation wire and hash stay frozen"
    );
    let violations = sqlx::query("PRAGMA foreign_key_check")
        .fetch_all(&pool)
        .await
        .unwrap();
    assert!(violations.is_empty());
    assert_schema_matches_fresh(&pool, "assistant migration").await;
}

#[tokio::test]
async fn assistant_provider_migration_corrupt_identity_fails_atomically() {
    for damage in [
        "UPDATE cards SET payload=json_set(payload,'$.assistant_provider','claude') WHERE id='assistant'",
        "UPDATE tracks SET root_session_id='current'",
        "UPDATE worker_sessions SET provider='claude' WHERE id='failed'",
        "UPDATE cards SET payload='{}' WHERE id='assistant'",
    ] {
        let (_dir, pool) = staged().await;
        sqlx::query(damage).execute(&pool).await.unwrap();
        let before: Vec<(String, String)> =
            sqlx::query_as("SELECT id,payload FROM cards ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        let error = calm_truth::MIGRATOR.run(&pool).await.unwrap_err();
        assert!(
            error.to_string().contains("invalid card/session identity"),
            "{error}"
        );
        let after: Vec<(String, String)> =
            sqlx::query_as("SELECT id,payload FROM cards ORDER BY id")
                .fetch_all(&pool)
                .await
                .unwrap();
        assert_eq!(before, after);
        let converted: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM worker_sessions WHERE contract='assistant'")
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(converted, 0);
    }
}
