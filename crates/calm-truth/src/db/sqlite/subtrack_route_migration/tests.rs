use super::*;
use crate::db::sqlite::{SqlxRepo, TrackWorkspacePlan, area_create_tx, track_create_tx};
use crate::model::{NewArea, NewTrack, RequestTheme};
use calm_types::report_blocks::{flat_text, render_fence};
use calm_types::track_report::ReportBlock;
use serde_json::json;

async fn fixture() -> (tempfile::TempDir, String, SqlxRepo) {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}", dir.path().join("migration.db").display());
    let repo = SqlxRepo::open(&url).await.unwrap();
    let mut tx = repo.pool().begin().await.unwrap();
    let area = area_create_tx(
        &mut tx,
        calm_types::organization::LEGACY_OWNER_USER_ID,
        NewArea {
            name: "migration".into(),
            color: "#000".into(),
            sort: None,
        },
    )
    .await
    .unwrap();
    track_create_tx(
        &mut tx,
        NewTrack {
            area_id: area.id,
            title: "migration".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .unwrap();
    sqlx::query("DELETE FROM report_data_migrations WHERE name=?1")
        .bind(NAME)
        .execute(&mut *tx)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    (dir, url, repo)
}

async fn seed_report(repo: &SqlxRepo, id: &str, bytes: Option<Vec<u8>>) -> TrackReportPayload {
    let block = ReportBlock {
        id: "b_1234".into(),
        rev: 7,
        kind: "task".into(),
        payload: json!({
        "key":"child","kind":"codex","goal":"sub-wave stays in prose",
        "ready":true,"declared_by":calm_types::report_blocks::tasks::PLANNER_DECLARATION_AUTHOR,"spawn":"sub-wave","context":{"spawn":"sub-wave"}}),
    };
    let mut payload = TrackReportPayload::new("sub-wave summary", flat_text(&block));
    payload.blocks = Some(vec![block]);
    let bytes = bytes.or_else(|| {
        Some(
            ReportDoc::from_blocks_exact(&payload.summary, payload.blocks.as_ref().unwrap())
                .unwrap()
                .to_bytes(),
        )
    });
    sqlx::query("INSERT INTO cards(id,track_id,kind,role,sort,payload,body_crdt,deletable,created_at,updated_at) \
        VALUES(?1,(SELECT id FROM tracks WHERE NOT EXISTS(\
        SELECT 1 FROM cards WHERE cards.track_id=tracks.id AND role='reportcard') LIMIT 1),'track-report','reportcard',0,?2,?3,0,1,1)")
        .bind(id).bind(serde_json::to_string(&payload).unwrap()).bind(bytes).execute(repo.pool()).await.unwrap();
    payload
}

#[tokio::test]
async fn subtrack_upgrade_through_repository_open_preserves_crdt_identity_and_evidence() {
    let (_dir, url, repo) = fixture().await;
    let before = seed_report(&repo, "report", None).await;
    sqlx::query("INSERT INTO tasks(id,track_id,key,kind,goal,context_json,spawn,status,created_at_ms,updated_at_ms) \
        VALUES('track:child',(SELECT id FROM tracks LIMIT 1),'child','codex','goal','{}','sub-wave','running',1,1)").execute(repo.pool()).await.unwrap();
    sqlx::query("INSERT INTO track_recipes(id,title,body,revision,created_at,updated_at) VALUES('recipe','recipe',?1,2,1,1)")
        .bind(&before.body).execute(repo.pool()).await.unwrap();
    let upgraded = SqlxRepo::open(&url).await.unwrap();
    let (raw, bytes): (String, Vec<u8>) =
        sqlx::query_as("SELECT payload,body_crdt FROM cards WHERE id='report'")
            .fetch_one(upgraded.pool())
            .await
            .unwrap();
    let after: TrackReportPayload = serde_json::from_str(&raw).unwrap();
    let doc = ReportDoc::from_bytes(&bytes).unwrap();
    assert_eq!(
        doc.project().unwrap(),
        (after.summary.clone(), after.body.clone())
    );
    assert_eq!(
        doc.blocks_snapshot().unwrap(),
        after.blocks.clone().unwrap()
    );
    let old = &before.blocks.unwrap()[0];
    let new = &after.blocks.unwrap()[0];
    assert_eq!((new.id.as_str(), new.rev), (old.id.as_str(), old.rev));
    assert_eq!(new.payload["spawn"], "sub-track");
    assert_eq!(new.payload["context"], old.payload["context"]);
    assert_eq!(new.payload["goal"], old.payload["goal"]);
    assert_eq!(after.summary, before.summary);
    assert_eq!(
        calm_types::subtrack_route::context_hash_preimage(new),
        flat_text(old)
    );
    let route: (String, String) =
        sqlx::query_as("SELECT spawn,status FROM tasks WHERE id='track:child'")
            .fetch_one(upgraded.pool())
            .await
            .unwrap();
    assert_eq!(route, ("sub-track".into(), "running".into()));
    let recipe: (String, i64) =
        sqlx::query_as("SELECT body,revision FROM track_recipes WHERE id='recipe'")
            .fetch_one(upgraded.pool())
            .await
            .unwrap();
    assert_eq!(recipe, (render_fence("task", &new.payload), 3));
    let _again = SqlxRepo::open(&url).await.unwrap();
    let repeated: (String, Vec<u8>) =
        sqlx::query_as("SELECT payload,body_crdt FROM cards WHERE id='report'")
            .fetch_one(upgraded.pool())
            .await
            .unwrap();
    assert_eq!(
        repeated,
        (raw, bytes),
        "repeated startup must be byte-idempotent"
    );
}

#[tokio::test]
async fn subtrack_upgrade_rolls_back_every_report_and_marker_on_corrupt_crdt() {
    let (_dir, url, repo) = fixture().await;
    seed_report(&repo, "a-good", None).await;
    let mut tx = repo.pool().begin().await.unwrap();
    let area_id: String = sqlx::query_scalar("SELECT area_id FROM tracks LIMIT 1")
        .fetch_one(&mut *tx)
        .await
        .unwrap();
    track_create_tx(
        &mut tx,
        NewTrack {
            area_id: area_id.into(),
            title: "second".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: RequestTheme::default_dark(),
        },
        None,
        &TrackWorkspacePlan::AttachedFromCwd,
        None,
        repo.track_area_cache(),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    seed_report(&repo, "z-bad", Some(vec![1, 2, 3])).await;
    let before: (String, Vec<u8>) =
        sqlx::query_as("SELECT payload,body_crdt FROM cards WHERE id='a-good'")
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert!(SqlxRepo::open(&url).await.is_err());
    let after: (String, Vec<u8>) =
        sqlx::query_as("SELECT payload,body_crdt FROM cards WHERE id='a-good'")
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(before, after, "earlier report update must roll back");
    let markers: i64 =
        sqlx::query_scalar("SELECT count(*) FROM report_data_migrations WHERE name=?1")
            .bind(NAME)
            .fetch_one(repo.pool())
            .await
            .unwrap();
    assert_eq!(markers, 0, "failed upgrade must not mark completion");
}

#[tokio::test]
async fn subtrack_upgrade_handles_payload_only_reports() {
    let (_dir, url, repo) = fixture().await;
    let before = seed_report(&repo, "report", None).await;
    sqlx::query("UPDATE cards SET body_crdt=NULL WHERE id='report'")
        .execute(repo.pool())
        .await
        .unwrap();
    let upgraded = SqlxRepo::open(&url).await.unwrap();
    let (raw, bytes): (String, Vec<u8>) =
        sqlx::query_as("SELECT payload,body_crdt FROM cards WHERE id='report'")
            .fetch_one(upgraded.pool())
            .await
            .unwrap();
    let after: TrackReportPayload = serde_json::from_str(&raw).unwrap();
    let doc = ReportDoc::from_bytes(&bytes).unwrap();
    assert_eq!(
        doc.blocks_snapshot().unwrap(),
        after.blocks.clone().unwrap()
    );
    assert_eq!(after.blocks.unwrap()[0].payload["spawn"], "sub-track");
    assert_eq!(
        doc.block_rev("b_1234").unwrap(),
        Some(before.blocks.unwrap()[0].rev)
    );
}
