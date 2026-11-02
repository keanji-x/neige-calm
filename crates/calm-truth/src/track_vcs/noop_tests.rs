use super::transcript_writer_tests::Fixture;
use super::*;
use crate::db::sqlite::{append_actor_events_in_tx, append_decision_events_in_tx, card_update_tx};
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use crate::model::CardPatch;
use serde_json::json;

async fn counts(fixture: &Fixture) -> (i64, i64, i64) {
    sqlx::query_as(
        "SELECT (SELECT COUNT(*) FROM events), \
                (SELECT COUNT(*) FROM track_vcs_commits), \
                (SELECT COUNT(*) FROM track_vcs_objects)",
    )
    .fetch_one(fixture.repo.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn unprojected_event_keeps_its_receipt_without_history_writes() {
    let f = Fixture::new().await;
    let before = counts(&f).await;
    let previous = head(f.repo.pool(), &f.track.id).await.unwrap();

    f.hook("new retained input").await;

    let after = counts(&f).await;
    assert_eq!(
        after.0,
        before.0 + 1,
        "the event receipt must remain durable"
    );
    assert_eq!(head(f.repo.pool(), &f.track.id).await.unwrap(), previous);
    assert_eq!((after.1, after.2), (before.1, before.2));
    // Hook events are intentionally projected at the turn boundary, even when
    // their immediate event append has no history delta.
    let refreshed = f.refresh().await;
    assert!(
        f.text(&refreshed, &f.card.id, "conversation.md")
            .await
            .contains("new retained input")
    );
}

#[tokio::test]
async fn unprojected_event_does_not_bootstrap_a_history_tree() {
    let f = Fixture::new().await;
    let write_db = (f.repo.db()).clone();
    write_db
        .writer()
        .with_writer(|write_session| {
            Box::pin(async move {
                let mut tx = write_session.begin_immediate().await.unwrap();
                sqlx::query("DELETE FROM track_vcs_refs WHERE track_id=?1")
                    .bind(f.track.id.as_str())
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                sqlx::query("DELETE FROM track_vcs_commits WHERE track_id=?1")
                    .bind(f.track.id.as_str())
                    .execute(&mut **tx)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
                let before = counts(&f).await;

                f.hook("before the first projection").await;

                let after = counts(&f).await;
                assert_eq!(after.0, before.0 + 1);
                assert_eq!(head(f.repo.pool(), &f.track.id).await.unwrap(), None);
                assert_eq!((after.1, after.2), (before.1, before.2));
                let refreshed = f.refresh().await;
                assert_eq!(
                    head(f.repo.pool(), &f.track.id).await.unwrap(),
                    Some(refreshed)
                );
            })
        })
        .await
        .expect("independent write operation")
}

#[tokio::test]
async fn mixed_event_batch_keeps_the_projected_change_and_hook() {
    let f = Fixture::new().await;
    let before = counts(&f).await;
    let previous = head(f.repo.pool(), &f.track.id).await.unwrap();
    let write_db = (f.repo.db()).clone();
    write_db.writer().with_writer(|write_session| Box::pin(async move {
        let mut tx = write_session.begin_immediate().await.unwrap();
        let updated = card_update_tx(
            &mut tx,
            f.card.id.as_str(),
            CardPatch {
                payload: Some(json!({"schemaVersion": 1, "task": "changed content"})),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let events = [
            Event::CodexHook {
                card_id: f.card.id.clone(),
                kind: "hook.codex.user_prompt_submit".into(),
                hook_idempotency_key: "mixed-hook".into(),
                payload: json!({"hook_event_name": "UserPromptSubmit", "prompt": "mixed retained input"}),
            },
            Event::CardUpdated(updated),
        ];
        let ids = append_decision_events_in_tx(
            &mut tx,
            &ActorId::Kernel,
            &EventScope::Card {
                card: f.card.id.clone(),
                track: f.track.id.clone(),
                area: f.track.area_id.clone(),
            },
            None,
            &events,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();

        let after = counts(&f).await;
        assert_eq!(ids.len(), 2);
        assert_eq!(after.0, before.0 + 2);
        assert_eq!(after.1, before.1 + 1);
        let current = head(f.repo.pool(), &f.track.id).await.unwrap();
        assert_ne!(current, previous);
        let commit = current.unwrap();
        assert!(
            f.text(&commit, &f.card.id, ".payload.json")
                .await
                .contains("changed content")
        );
        assert!(
            f.text(&commit, &f.card.id, "conversation.md")
                .await
                .contains("mixed retained input")
        );

    })).await.expect("independent write operation")
}

fn presentation(f: &Fixture) -> Event {
    use calm_types::conversation_display::{ConversationCursor, ConversationCursorSource};
    Event::HarnessPresentationChanged {
        card_id: f.card.id.clone(),
        track_id: f.track.id.clone(),
        cursor: ConversationCursor::new(
            f.card.id.as_str(),
            ConversationCursorSource::Presentation,
            1,
        ),
    }
}

#[tokio::test]
async fn unchanged_projection_keeps_receipts_and_head_without_history_writes() {
    let f = Fixture::new().await;
    f.refresh().await;
    let before = counts(&f).await;
    let reference: (String, Option<i64>) =
        sqlx::query_as("SELECT head_hash, updated_event_id FROM track_vcs_refs WHERE track_id=?1")
            .bind(f.track.id.as_str())
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    let events = [
        presentation(&f),
        Event::OverlaySet(crate::model::Overlay {
            id: "unchanged-overlay".into(),
            plugin_id: "test".into(),
            entity_kind: "card".into(),
            entity_id: f.card.id.to_string(),
            kind: "badge".into(),
            payload: json!({"text": "running"}),
            updated_at: 1,
        }),
    ];
    let (_, _, database): (i64, String, String) = sqlx::query_as("PRAGMA database_list")
        .fetch_one(f.repo.pool())
        .await
        .unwrap();
    f.repo
        .db()
        .writer()
        .execute(sqlx::query("PRAGMA wal_checkpoint(TRUNCATE)"))
        .await
        .unwrap();
    let started = std::time::Instant::now();
    for event in events.iter().cycle().take(32) {
        let id = f.log(event.clone()).await;
        let receipt: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE id=?1")
            .bind(id)
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
        assert_eq!(receipt, 1);
    }
    let after = counts(&f).await;
    let elapsed = started.elapsed();
    let wal = std::fs::metadata(format!("{database}-wal")).unwrap().len();
    eprintln!(
        "unchanged workload: receipts={}, commits={}, objects={}, wal_bytes={wal}, elapsed_us={}",
        after.0 - before.0,
        after.1 - before.1,
        after.2 - before.2,
        elapsed.as_micros()
    );
    assert_eq!(after.0, before.0 + 32);
    assert_eq!((after.1, after.2), (before.1, before.2));
    let current: (String, Option<i64>) =
        sqlx::query_as("SELECT head_hash, updated_event_id FROM track_vcs_refs WHERE track_id=?1")
            .bind(f.track.id.as_str())
            .fetch_one(f.repo.pool())
            .await
            .unwrap();
    assert_eq!(current, reference);
}

// A ref UPDATE that writes the same values is still a forbidden history write.
async fn forbid_history_writes(f: &Fixture) {
    for statement in [
        "CREATE TRIGGER reject_commit BEFORE INSERT ON track_vcs_commits \
         BEGIN SELECT RAISE(ABORT, 'unexpected history insert'); END",
        "CREATE TRIGGER reject_ref BEFORE UPDATE ON track_vcs_refs \
         BEGIN SELECT RAISE(ABORT, 'unexpected ref update'); END",
        "CREATE TRIGGER reject_object BEFORE INSERT ON track_vcs_objects \
         BEGIN SELECT RAISE(ABORT, 'unexpected object insert'); END",
    ] {
        f.repo
            .db()
            .writer()
            .execute(sqlx::query(statement))
            .await
            .unwrap();
    }
}

fn write_context(f: &Fixture) -> crate::state::WriteContext {
    crate::state::WriteContext::new(
        f.repo.card_role_cache().clone(),
        f.repo.track_area_cache().clone(),
    )
}

fn scope(f: &Fixture) -> EventScope {
    EventScope::Card {
        card: f.card.id.clone(),
        track: f.track.id.clone(),
        area: f.track.area_id.clone(),
    }
}

#[tokio::test]
async fn unchanged_projection_returns_existing_identity_to_actor_batch_callers() {
    let f = Fixture::new().await;
    let previous = f.refresh().await;
    let before = counts(&f).await;
    forbid_history_writes(&f).await;
    let write_db = f.repo.db().clone();
    write_db
        .writer()
        .with_writer(|session| {
            Box::pin(async {
                let mut tx = session.begin_immediate().await.unwrap();
                let events = [
                    (ActorId::Kernel, scope(&f), presentation(&f)),
                    (
                        ActorId::Kernel,
                        scope(&f),
                        Event::CardUpdated(f.card.clone()),
                    ),
                ];
                let (ids, commits) =
                    append_actor_events_in_tx(&mut tx, None, &write_context(&f), &events)
                        .await
                        .unwrap();
                assert_eq!(ids.len(), 2);
                assert_eq!(commits.get(&f.track.id), Some(&previous));
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
    let after = counts(&f).await;
    assert_eq!(after, (before.0 + 2, before.1, before.2));
}

#[tokio::test]
async fn same_cursor_projection_publishes_real_changes_with_event_and_author() {
    let f = Fixture::new().await;
    let mut previous = f.refresh().await;
    let mut first_tree = None;
    for content in ["first", "second", "first"] {
        let write_db = f.repo.db().clone();
        let current = write_db
            .writer()
            .with_writer(|session| {
                Box::pin(async {
                    let mut tx = session.begin_immediate().await.unwrap();
                    card_update_tx(
                        &mut tx,
                        f.card.id.as_str(),
                        CardPatch {
                            payload: Some(json!({"schemaVersion": 1, "task": content})),
                            ..Default::default()
                        },
                    )
                    .await
                    .unwrap();
                    // Hold metadata fixed so returning to the first payload restores its exact tree.
                    sqlx::query("UPDATE cards SET updated_at=?1 WHERE id=?2")
                        .bind(f.card.updated_at)
                        .bind(f.card.id.as_str())
                        .execute(&mut **tx)
                        .await
                        .unwrap();
                    let (ids, commits) = append_actor_events_in_tx(
                        &mut tx,
                        None,
                        &write_context(&f),
                        &[(ActorId::Kernel, scope(&f), presentation(&f))],
                    )
                    .await
                    .unwrap();
                    let hash = commits[&f.track.id].clone();
                    tx.commit().await.unwrap();
                    (hash, ids[0])
                })
            })
            .await
            .unwrap();
        assert_ne!(current.0, previous);
        let record = commit_record(f.repo.pool(), &current.0)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(record.parent_hash.as_deref(), Some(previous.as_str()));
        assert_eq!(record.event_id, Some(current.1));
        assert_eq!(record.author, Some(ActorId::Kernel.to_string()));
        if content == "first" {
            if let Some(tree) = &first_tree {
                assert_eq!(&record.tree_hash, tree);
            } else {
                first_tree = Some(record.tree_hash.clone());
            }
        }
        assert!(
            f.text(&current.0, &f.card.id, ".payload.json")
                .await
                .contains(content)
        );
        f.log(presentation(&f)).await;
        assert_eq!(
            head(f.repo.pool(), &f.track.id).await.unwrap(),
            Some(current.0.clone())
        );
        previous = current.0;
    }
}

#[tokio::test]
async fn unchanged_projection_repairs_missing_blob_before_reusing_head() {
    let f = Fixture::new().await;
    let previous = f.refresh().await;
    let before = counts(&f).await;
    let manifest = tree_at(f.repo.pool(), &previous).await.unwrap().unwrap();
    let path = format!("cards/{}/.payload.json", f.card.id);
    f.repo
        .db()
        .writer()
        .execute(
            sqlx::query("DELETE FROM track_vcs_objects WHERE hash=?1")
                .bind(&manifest.entries[&path].blob_hash),
        )
        .await
        .unwrap();
    assert!(cat_at(f.repo.pool(), &previous, &path).await.is_err());
    f.log(presentation(&f)).await;
    assert_eq!(
        head(f.repo.pool(), &f.track.id).await.unwrap(),
        Some(previous.clone())
    );
    assert!(
        cat_at(f.repo.pool(), &previous, &path)
            .await
            .unwrap()
            .content
            .contains("schemaVersion")
    );
    assert_eq!(counts(&f).await, (before.0 + 1, before.1, before.2));
}

#[tokio::test]
async fn projection_bootstraps_missing_head_and_schema_changes_create_history() {
    let f = Fixture::new().await;
    f.repo
        .db()
        .writer()
        .execute(
            sqlx::query("DELETE FROM track_vcs_refs WHERE track_id=?1").bind(f.track.id.as_str()),
        )
        .await
        .unwrap();
    f.log(presentation(&f)).await;
    let previous = head(f.repo.pool(), &f.track.id).await.unwrap().unwrap();
    assert_eq!(
        commit_record(f.repo.pool(), &previous)
            .await
            .unwrap()
            .unwrap()
            .parent_hash,
        None
    );
    let write_db = f.repo.db().clone();
    let next = write_db
        .writer()
        .with_writer(|session| {
            Box::pin(async {
                let mut tx = session.begin_immediate().await.unwrap();
                let hash = commit_in_tx(
                    &mut tx,
                    &f.track.id,
                    &ActorId::Kernel,
                    42,
                    &presentation(&f),
                    MANIFEST_SCHEMA_VERSION + 1,
                )
                .await
                .unwrap()
                .unwrap();
                tx.commit().await.unwrap();
                hash
            })
        })
        .await
        .unwrap();
    assert_ne!(next, previous);
    assert_eq!(
        tree_at(f.repo.pool(), &next)
            .await
            .unwrap()
            .unwrap()
            .schema_version,
        MANIFEST_SCHEMA_VERSION + 1
    );
}

#[tokio::test]
async fn rolled_back_projection_keeps_receipts_content_and_head_atomic() {
    let f = Fixture::new().await;
    let previous = f.refresh().await;
    let before = counts(&f).await;
    let write_db = f.repo.db().clone();
    write_db
        .writer()
        .with_writer(|session| {
            Box::pin(async {
                for changed in [false, true] {
                    let mut tx = session.begin_immediate().await.unwrap();
                    if changed {
                        card_update_tx(
                            &mut tx,
                            f.card.id.as_str(),
                            CardPatch {
                                payload: Some(json!({"schemaVersion": 1, "task": "rolled back"})),
                                ..Default::default()
                            },
                        )
                        .await
                        .unwrap();
                    }
                    let (_, commits) = append_actor_events_in_tx(
                        &mut tx,
                        None,
                        &write_context(&f),
                        &[(ActorId::Kernel, scope(&f), presentation(&f))],
                    )
                    .await
                    .unwrap();
                    assert_eq!(commits[&f.track.id] != previous, changed);
                    tx.rollback().await.unwrap();
                    assert_eq!(counts(&f).await, before);
                    assert_eq!(
                        head(f.repo.pool(), &f.track.id).await.unwrap(),
                        Some(previous.clone())
                    );
                }
            })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn unchanged_projection_uses_current_head_when_other_writers_publish_content() {
    let f = Fixture::new().await;
    let previous = f.refresh().await;
    let before = counts(&f).await;
    let write_db = f.repo.db().clone();
    let update = write_db.writer().with_writer(|session| {
        Box::pin(async {
            let mut tx = session.begin_immediate().await.unwrap();
            card_update_tx(
                &mut tx,
                f.card.id.as_str(),
                CardPatch {
                    payload: Some(json!({"schemaVersion": 1, "task": "concurrent content"})),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let (_, commits) = append_actor_events_in_tx(
                &mut tx,
                None,
                &write_context(&f),
                &[(ActorId::Kernel, scope(&f), presentation(&f))],
            )
            .await
            .unwrap();
            tx.commit().await.unwrap();
            commits[&f.track.id].clone()
        })
    });
    let notifications = async {
        for _ in 0..8 {
            f.log(presentation(&f)).await;
        }
    };
    let (updated, ()) = tokio::join!(update, notifications);
    let updated = updated.unwrap();
    assert_ne!(updated, previous);
    assert_eq!(
        head(f.repo.pool(), &f.track.id).await.unwrap(),
        Some(updated.clone())
    );
    assert!(
        f.text(&updated, &f.card.id, ".payload.json")
            .await
            .contains("concurrent content")
    );
    let after = counts(&f).await;
    assert_eq!(after.0, before.0 + 9);
    assert_eq!(after.1, before.1 + 1);
}
