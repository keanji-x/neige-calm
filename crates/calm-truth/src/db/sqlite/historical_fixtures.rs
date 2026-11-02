//! Fixed graph seed for migration tests starting after 0166 and before 0210.
//! A historical database cannot use today's Track writer or retirement columns.
use sqlx::{Sqlite, Transaction};

pub(super) async fn card_before_retirement(
    tx: &mut Transaction<'_, Sqlite>,
    kind: &str,
    role: crate::model::CardRole,
    payload: serde_json::Value,
) -> (String, String, String) {
    let area = crate::model::new_id();
    let track = crate::model::new_id();
    let card = crate::model::new_id();
    let resource = crate::model::new_id();
    sqlx::query("INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES(?,'historical','#123',0,1,1)")
        .bind(&area).execute(&mut **tx).await.unwrap();
    sqlx::query("INSERT INTO tracks(id,area_id,title,sort,workspace_kind,workspace_path,created_at,updated_at) VALUES(?,?,'historical',0,'attached','/tmp',1,1)")
        .bind(&track).bind(&area).execute(&mut **tx).await.unwrap();
    sqlx::query("INSERT INTO workspace_resources(id,source_track_id,root_path,ownership,observation_version,created_at_ms) VALUES(?,?,'/tmp','borrowed',0,1)")
        .bind(&resource).bind(&track).execute(&mut **tx).await.unwrap();
    sqlx::query("INSERT INTO track_workspace_bindings(track_id,workspace_id,selection_version,selected_path,ownership) VALUES(?,?,1,'/tmp','borrowed')")
        .bind(&track).bind(&resource).execute(&mut **tx).await.unwrap();
    sqlx::query("INSERT INTO cards(id,track_id,kind,sort,payload,title,deletable,created_at,updated_at,role) VALUES(?,?,?,0,?,'historical',1,1,1,?)")
        .bind(&card).bind(&track).bind(kind).bind(payload.to_string()).bind(role.as_db_str())
        .execute(&mut **tx).await.unwrap();
    (area, track, card)
}

pub(super) async fn conversation_card(repo: &super::SqlxRepo) -> (String, String) {
    repo.db()
        .writer()
        .with_writer(|session| {
            Box::pin(async move {
                let mut tx = session.begin_immediate().await.unwrap();
                let (area, _, card) = card_before_retirement(
                    &mut tx,
                    "codex",
                    crate::model::CardRole::Planner,
                    serde_json::json!({"planner_provider":"codex"}),
                )
                .await;
                tx.commit().await.unwrap();
                (area, card)
            })
        })
        .await
        .unwrap()
}
