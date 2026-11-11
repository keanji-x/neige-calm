//! Fixed migration inputs for released storage contracts: cards before 0210
//! and authenticated messages before 0217 use their exact historical columns.
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

/// Frozen authenticated-message columns from 0175, before 0217 added guest attribution.
pub(super) async fn insert_message_before_guest_tx(
    tx: &mut Transaction<'_, Sqlite>,
    message: &super::conversation::StoredMessage,
    revision: &super::conversation::StoredMessageRevision,
) {
    let calm_types::conversation::MessageAuthor::AuthenticatedHuman { registered_user_id } =
        &message.author
    else {
        panic!("guest data has no pre-0217 encoding")
    };
    sqlx::query(
        "INSERT INTO conversation_messages(id,card_id,current_revision,author_kind,
        author_user_id,created_at_ms,withdrawn_at_ms)
        VALUES(?1,?2,?3,'authenticated_human',?4,?5,?6)",
    )
    .bind(message.current.message_id.as_str())
    .bind(&message.card_id)
    .bind(i64::from(message.current.revision.get()))
    .bind(registered_user_id)
    .bind(message.created_at_ms)
    .bind(message.withdrawn_at_ms)
    .execute(&mut **tx)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO conversation_message_revisions(message_id,revision,body_sha256,
        payload_json,created_at_ms) VALUES(?1,?2,?3,?4,?5)",
    )
    .bind(revision.reference.message_id.as_str())
    .bind(i64::from(revision.reference.revision.get()))
    .bind(&revision.body_sha256)
    .bind(serde_json::to_string(&revision.payload).unwrap())
    .bind(revision.created_at_ms)
    .execute(&mut **tx)
    .await
    .unwrap();
}

pub(super) async fn owner_message_before_guest_tx(
    tx: &mut Transaction<'_, Sqlite>,
    card: &str,
    payload: calm_types::conversation::HumanPayload,
) -> calm_types::conversation::MessageRevisionRef {
    use calm_types::conversation::*;
    let reference = MessageRevisionRef {
        message_id: ConversationMessageId::parse(&crate::model::new_id()).unwrap(),
        revision: std::num::NonZeroU32::MIN,
    };
    let message = super::conversation::StoredMessage {
        current: reference.clone(),
        card_id: card.into(),
        author: MessageAuthor::AuthenticatedHuman {
            registered_user_id: calm_types::organization::LEGACY_OWNER_USER_ID.into(),
        },
        created_at_ms: 1,
        withdrawn_at_ms: None,
    };
    let revision = super::conversation::StoredMessageRevision {
        reference: reference.clone(),
        body_sha256: super::conversation::digest(&payload).unwrap(),
        payload: PrivateHistoryPayload::Present { value: payload },
        created_at_ms: 1,
    };
    insert_message_before_guest_tx(tx, &message, &revision).await;
    reference
}
