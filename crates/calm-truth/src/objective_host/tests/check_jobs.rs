use super::super::check_jobs;
use super::*;
use calm_types::track_report::ReportBlock;
use serde_json::json;

async fn evaluator_job(repo: &SqlxRepo) -> StoredIntent {
    let mut d = definition();
    d.checks[0].config=serde_json::from_value(json!({"kind":"evaluator","params":{"rubric":[{"id":"quality","criterion":"Correct behavior"}],"samples":1,"aggregate":"majority"}})).unwrap();
    for (i, event) in [
        Event::DefinitionChanged {
            objective: d.clone(),
        },
        Event::ApprovalGranted {
            objective: d.key.clone(),
            approval: Approval {
                standard_hash: standard_hash(&d),
                approver: "human".into(),
            },
        },
        Event::Requested {
            objective: d.key,
            revision: Revision::new("r0"),
        },
    ]
    .into_iter()
    .enumerate()
    {
        fixture_react(repo, event, &format!("eval-{i}")).await;
    }
    unsettled_intents(repo.db())
        .await
        .unwrap()
        .into_iter()
        .find(|r| matches!(r.intent, Intent::RunCheck { .. }))
        .unwrap()
}
fn block() -> ReportBlock {
    ReportBlock {
        id: "normal-report-id".into(),
        kind: "task".into(),
        rev: 1,
        payload: json!({"key":"objcheck-test","goal":"fixed"}),
    }
}
#[tokio::test]
async fn check_jobs_bind_pending_and_stale_current_standard_admission() {
    let repo = fixture().await;
    let job = evaluator_job(&repo).await;
    // A new request supersedes a still-pending unbound job. This must be refused
    // by binding admission independently of the pending-only lifecycle CAS.
    fixture_react(
        &repo,
        Event::Requested {
            objective: definition().key,
            revision: Revision::new("r1"),
        },
        "next",
    )
    .await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert!(
        check_jobs::bind_tx(&mut tx, &job.id, "objcheck-test", &block().payload)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let current = unsettled_intents(repo.db())
        .await
        .unwrap()
        .into_iter()
        .find(|r| matches!(&r.intent,Intent::RunCheck{revision,..} if revision.as_str()=="r1"))
        .unwrap();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    check_jobs::bind_tx(&mut tx, &current.id, "objcheck-test", &block().payload)
        .await
        .unwrap();
    assert!(
        check_jobs::bind_tx(&mut tx, &current.id, "objcheck-test", &block().payload)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM objective_check_jobs")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        0
    );
}
#[tokio::test]
async fn check_jobs_guard_payload_hash_and_unrelated_writes() {
    let repo = fixture().await;
    let job = evaluator_job(&repo).await;
    let b = block();
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    check_jobs::bind_tx(&mut tx, &job.id, "objcheck-test", &b.payload)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let mut changed = b.clone();
    changed.payload["goal"] = json!("injected");
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert!(
        check_jobs::guard_report_tx(
            &mut tx,
            &"t".into(),
            std::slice::from_ref(&b),
            std::slice::from_ref(&changed)
        )
        .await
        .is_err()
    );
    check_jobs::guard_report_tx(
        &mut tx,
        &"t".into(),
        std::slice::from_ref(&b),
        std::slice::from_ref(&b),
    )
    .await
    .unwrap();
    let mut unrelated = b.clone();
    unrelated.payload["key"] = json!("ordinary");
    check_jobs::guard_report_tx(&mut tx, &"t".into(), &[], &[unrelated])
        .await
        .unwrap();
    let mut tombstone = b.clone();
    tombstone.payload = json!({"key":"objcheck-test","tombstone":{"reason":"withdrawn"}});
    check_jobs::guard_report_tx(&mut tx, &"t".into(), &[b], &[tombstone])
        .await
        .unwrap();
    tx.rollback().await.unwrap();
}
#[tokio::test]
async fn check_jobs_migrations_append_only_foreign_key_and_cascade() {
    let repo = fixture().await;
    let job = evaluator_job(&repo).await;
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    check_jobs::bind_tx(&mut tx, &job.id, "objcheck-test", &block().payload)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    for sql in [
        "UPDATE objective_check_jobs SET task_key='changed'",
        "DELETE FROM objective_check_jobs",
        "INSERT OR REPLACE INTO objective_check_jobs SELECT * FROM objective_check_jobs",
    ] {
        assert!(
            sqlx::query(sql).execute(repo.pool()).await.is_err(),
            "{sql}"
        );
    }
    assert!(
        sqlx::query("INSERT INTO objective_check_jobs VALUES('t',999,0,'missing','hash')")
            .execute(repo.pool())
            .await
            .is_err()
    );
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    track_delete_tx(
        &mut tx,
        "t",
        &crate::track_area_cache::TrackAreaCache::new(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT count(*) FROM objective_check_jobs")
            .fetch_one(repo.pool())
            .await
            .unwrap(),
        0
    );
}

#[tokio::test]
async fn check_jobs_migrations_upgrade_adds_only_binding_storage() {
    use sqlx::{Connection, SqliteConnection};
    let mut db = SqliteConnection::connect("sqlite::memory:").await.unwrap();
    let before = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|m| m.version < 192)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    before.run(&mut db).await.unwrap();
    let old: Vec<(String, String)> =
        sqlx::query_as("SELECT name,sql FROM sqlite_master WHERE sql IS NOT NULL ORDER BY name")
            .fetch_all(&mut db)
            .await
            .unwrap();
    let through_binding = sqlx::migrate::Migrator {
        migrations: std::borrow::Cow::Owned(
            crate::MIGRATOR
                .iter()
                .filter(|m| m.version <= 192)
                .cloned()
                .collect(),
        ),
        ..sqlx::migrate::Migrator::DEFAULT
    };
    through_binding.run(&mut db).await.unwrap();
    let preserved: Vec<(String,String)> = sqlx::query_as("SELECT name,sql FROM sqlite_master WHERE sql IS NOT NULL AND name NOT LIKE 'objective_check_jobs%' ORDER BY name").fetch_all(&mut db).await.unwrap();
    assert_eq!(
        old, preserved,
        "released schema and snapshot head stay byte-identical"
    );
    let additions: Vec<(String, String)> = sqlx::query_as(
        "SELECT type,name FROM sqlite_master WHERE name LIKE 'objective_check_jobs%' ORDER BY name",
    )
    .fetch_all(&mut db)
    .await
    .unwrap();
    assert_eq!(additions.len(), 4);
    assert_eq!(
        additions.iter().filter(|(kind, _)| kind == "table").count(),
        1
    );
}

#[tokio::test]
async fn check_jobs_bind_changed_standard_rejects_unbound_pending_intent() {
    let repo = fixture().await;
    let job = evaluator_job(&repo).await;
    let (_, state) = load_state(repo.db(), &"t".into()).await.unwrap();
    let mut changed = state.objectives[&definition().key].definition.clone();
    changed.constraints.push("Preserve public behavior".into());
    fixture_react(
        &repo,
        Event::DefinitionChanged { objective: changed },
        "changed-standard",
    )
    .await;
    let pending = unsettled_intents(repo.db()).await.unwrap();
    assert!(
        pending
            .iter()
            .any(|row| row.id == job.id && row.status == IntentStatus::Pending)
    );
    let mut tx = begin_immediate_tx(repo.db()).await.unwrap();
    assert!(matches!(
        check_jobs::bind_tx(&mut tx, &job.id, "objcheck-test", &block().payload).await,
        Err(error) if error.to_string().contains("objective check binding is no longer current")
    ));
    tx.rollback().await.unwrap();
}
