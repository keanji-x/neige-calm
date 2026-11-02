//! Retirement reservations use the real resource writer and survive owner removal.
use super::*;
use crate::db::sqlite::workspace_retirement::*;
use crate::error::CalmError;
use calm_types::error::CoreError;
use std::path::Path;

fn retired(result: crate::error::Result<()>) {
    assert!(matches!(
        result,
        Err(CalmError::Core(CoreError::BadRequest(_)))
    ));
}

#[tokio::test]
async fn retirement_owned_selection_is_atomic_and_cannot_be_rebound() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let track = create(
        &repo,
        "",
        TrackWorkspacePlan::ManagedUnder(root.path().into()),
    )
    .await;
    let other = create(
        &repo,
        "",
        TrackWorkspacePlan::ManagedUnder(root.path().into()),
    )
    .await;
    let pool = repo.pool().clone();
    repo.db()
        .writer()
        .with_writer(|session| {
            Box::pin(async move {
                let path = Path::new(track.workspace.agent_cwd());
                let mut tx = session.begin_immediate().await.unwrap();
                retire_workspace_selection_tx(&mut tx, track.id.as_str())
                    .await
                    .unwrap();
                retired(require_active_workspace_path(&mut tx, path).await);
                tx.rollback().await.unwrap();
                require_active_workspace_path(&mut pool.acquire().await.unwrap(), path)
                    .await
                    .unwrap();
                let mut tx = session.begin_immediate().await.unwrap();
                retire_workspace_selection_tx(&mut tx, track.id.as_str())
                    .await
                    .unwrap();
                retire_workspace_selection_tx(&mut tx, track.id.as_str())
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
                let mut tx = session.begin_immediate().await.unwrap();
                retired(
                    track_workspace_write_tx(&mut tx, track.id.as_str(), &track.workspace).await,
                );
                tx.rollback().await.unwrap();
                let mut tx = session.begin_immediate().await.unwrap();
                retired(
                    track_workspace_write_tx(&mut tx, other.id.as_str(), &track.workspace).await,
                );
                tx.rollback().await.unwrap();
            })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn retirement_borrowed_repository_remains_available() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let track = create(
        &repo,
        root.path().to_str().unwrap(),
        TrackWorkspacePlan::AttachedFromCwd,
    )
    .await;
    repo.db()
        .writer()
        .with_writer(|session| {
            Box::pin(async move {
                let mut tx = session.begin_immediate().await.unwrap();
                retire_workspace_selection_tx(&mut tx, track.id.as_str())
                    .await
                    .unwrap();
                require_active_workspace_path(&mut tx, root.path())
                    .await
                    .unwrap();
                assert!(retired_workspaces(&mut tx).await.unwrap().is_empty());
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn retirement_extra_provision_rejects_stale_and_mismatched_identity() {
    use crate::db::sqlite::append_decision_event_in_tx;
    use crate::{
        event::{Event, EventScope},
        ids::ActorId,
    };
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let root = tempfile::tempdir().unwrap();
    repo.db()
        .writer()
        .with_writer(|session| {
            Box::pin(async move {
                let mut tx = session.begin_immediate().await.unwrap();
                let scope = EventScope::Card {
                    card: "card".into(),
                    track: "owner".into(),
                    area: "area".into(),
                };
                let event = Event::WorktreeProvisioned {
                    track_id: "owner".into(),
                    card_id: "card".into(),
                    path: root.path().to_str().unwrap().into(),
                };
                let old = append_decision_event_in_tx(
                    &mut tx,
                    &ActorId::KernelDispatcher,
                    &scope,
                    None,
                    &event,
                )
                .await
                .unwrap();
                let current = append_decision_event_in_tx(
                    &mut tx,
                    &ActorId::KernelDispatcher,
                    &scope,
                    None,
                    &event,
                )
                .await
                .unwrap();
                assert!(
                    !retire_extra_worktree_tx(&mut tx, old, "owner", root.path())
                        .await
                        .unwrap()
                );
                require_active_workspace_path(&mut tx, root.path())
                    .await
                    .unwrap();
                assert!(matches!(
                    retire_extra_worktree_tx(&mut tx, current, "other", root.path()).await,
                    Err(CalmError::Core(CoreError::Conflict(_)))
                ));
                assert!(
                    retire_extra_worktree_tx(&mut tx, current, "owner", root.path())
                        .await
                        .unwrap()
                );
                assert!(
                    retire_extra_worktree_tx(&mut tx, current, "owner", root.path())
                        .await
                        .unwrap()
                );
                retired(require_active_workspace_path(&mut tx, root.path()).await);
                assert_eq!(retired_workspaces(&mut tx).await.unwrap().len(), 1);
                tx.commit().await.unwrap();
            })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn retirement_reservation_survives_database_reopen_and_owner_deletion() {
    let root = tempfile::tempdir().unwrap();
    let url = format!("sqlite:{}", root.path().join("state.db").display());
    let repo = SqlxRepo::open(&url).await.unwrap();
    let track = create(
        &repo,
        "",
        TrackWorkspacePlan::ManagedUnder(root.path().join("workspaces")),
    )
    .await;
    let path = Path::new(track.workspace.agent_cwd()).to_path_buf();
    crate::db::RepoSyncDomainRaw::track_delete(&repo, track.id.as_str())
        .await
        .unwrap();
    repo.pool().close().await;
    let reopened = SqlxRepo::open(&url).await.unwrap();
    retired(
        require_active_workspace_path(&mut reopened.pool().acquire().await.unwrap(), &path).await,
    );
}

#[cfg(unix)]
#[tokio::test]
async fn retirement_damaged_garbage_does_not_gate_unrelated_admission() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let track = create(
        &repo,
        "",
        TrackWorkspacePlan::ManagedUnder(root.path().join("managed")),
    )
    .await;
    let path = Path::new(track.workspace.agent_cwd()).to_path_buf();
    std::fs::create_dir_all(&path).unwrap();
    let unrelated = root.path().join("unrelated");
    std::fs::create_dir(&unrelated).unwrap();
    let cache = repo.track_area_cache().clone();
    repo.db()
        .writer()
        .with_writer(|session| {
            Box::pin(async move {
                let mut tx = session.begin_immediate().await.unwrap();
                retire_workspace_selection_tx(&mut tx, track.id.as_str())
                    .await
                    .unwrap();
                crate::db::sqlite::track_delete_tx(&mut tx, track.id.as_str(), &cache)
                    .await
                    .unwrap();
                tx.commit().await.unwrap();
                std::fs::remove_dir(&path).unwrap();
                std::os::unix::fs::symlink(path.with_file_name("missing"), &path).unwrap();
                let mut tx = session.begin_immediate().await.unwrap();
                require_active_workspace_path(&mut tx, &unrelated)
                    .await
                    .unwrap();
                crate::db::sqlite::admit_attached_workspace_tx(
                    &mut tx,
                    unrelated.to_str().unwrap(),
                    &[],
                    &[],
                    &TrackWorkspacePlan::AttachedFromCwd,
                )
                .await
                .unwrap();
                tx.rollback().await.unwrap();
            })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn retirement_extra_at_primary_or_ancestor_keeps_primary_available() {
    use crate::db::sqlite::append_decision_event_in_tx;
    use crate::{
        event::{Event, EventScope},
        ids::ActorId,
    };
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let track = create(
        &repo,
        "",
        TrackWorkspacePlan::ManagedUnder(root.path().into()),
    )
    .await;
    repo.db()
        .writer()
        .with_writer(|session| {
            Box::pin(async move {
                let mut tx = session.begin_immediate().await.unwrap();
                let primary = Path::new(track.workspace.agent_cwd());
                let mut receipts = Vec::new();
                for path in [primary, primary.parent().unwrap()] {
                    let id = append_decision_event_in_tx(
                        &mut tx,
                        &ActorId::KernelDispatcher,
                        &EventScope::Card {
                            card: "card".into(),
                            track: track.id.clone(),
                            area: track.area_id.clone(),
                        },
                        None,
                        &Event::WorktreeProvisioned {
                            track_id: track.id.clone(),
                            card_id: "card".into(),
                            path: path.to_str().unwrap().into(),
                        },
                    )
                    .await
                    .unwrap();
                    receipts.push((id, path));
                }
                retire_track_extras_tx(&mut tx, track.id.as_str())
                    .await
                    .unwrap();
                require_active_workspace_path(&mut tx, primary)
                    .await
                    .unwrap();
                assert!(retired_workspaces(&mut tx).await.unwrap().is_empty());
                // Old preflight could leave protected primary claims parked with
                // no admitted execution. Upgrade inventory must agree with the
                // new registration policy, including the ancestor case.
                for (id, path) in receipts {
                    let operation = format!("legacy:{id}");
                    let payload = serde_json::json!({"target": {"track_id": track.id,
                        "provision_id": id, "request": {"path": path}}});
                    sqlx::query("INSERT INTO operations(id,operation_key,kind,payload_hash,target_type,target_json,payload_json,tx_output_json,phase,created_at_ms,updated_at_ms,parked_at_ms,parked_deadline_ms) VALUES (?1,?1,'workspace-reclaim','hash','track','{}',?2,'{\"data\":{\"workspace_reclaim\":{\"execution_id\":null}}}','parked',1,1,1,2)")
                        .bind(operation).bind(payload.to_string()).execute(&mut **tx).await.unwrap();
                }
                require_active_workspace_path(&mut tx, primary).await.unwrap();
                tx.rollback().await.unwrap();
            })
        })
        .await
        .unwrap();
}

#[tokio::test]
async fn retirement_corrupt_unadmitted_target_does_not_gate_unrelated_execution() {
    unadmitted_target(None).await;
}

#[tokio::test]
async fn retirement_unadmitted_receipt_does_not_require_card_metadata() {
    unadmitted_target(Some("card")).await;
}

async fn unadmitted_target(damage: Option<&'static str>) {
    use crate::db::sqlite::append_decision_event_in_tx;
    use crate::{
        event::{Event, EventScope},
        ids::ActorId,
    };
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let root = tempfile::tempdir().unwrap();
    repo.db().writer().with_writer(|session| Box::pin(async move {
        let mut tx = session.begin_immediate().await.unwrap();
        let event = append_decision_event_in_tx(&mut tx, &ActorId::KernelDispatcher,
            &EventScope::Card {card: "card".into(),track: "owner".into(),area: "area".into()}, None,
            &Event::WorktreeProvisioned {track_id: "owner".into(),card_id: "card".into(),path: root.path().join("extra").to_str().unwrap().into()}).await.unwrap();
        if damage == Some("card") {
            sqlx::query("UPDATE events SET payload=json_remove(payload,'$.card_id') WHERE id=?")
                .bind(event).execute(&mut **tx).await.unwrap();
        }
        if damage == Some("path") {
            sqlx::query("UPDATE events SET payload=json_remove(payload,'$.path') WHERE id=?")
                .bind(event).execute(&mut **tx).await.unwrap();
        } else if damage == Some("json") {
            sqlx::query("UPDATE events SET payload='unreadable' WHERE id=?")
                .bind(event).execute(&mut **tx).await.unwrap();
        }
        if let Some(path) = match damage {
            Some("empty") => Some(""),
            Some("relative") => Some(".."),
            _ => None,
        } {
            sqlx::query("UPDATE events SET payload=json_set(payload,'$.path',?) WHERE id=?")
                .bind(path).bind(event).execute(&mut **tx).await.unwrap();
        }
        let payload = serde_json::json!({"target":{"track_id":"incorrect-owner","provision_id":event,"request":{"path":"/incorrect/path"}}});
        sqlx::query("INSERT INTO operations(id,operation_key,kind,payload_hash,target_type,target_json,payload_json,tx_output_json,phase,created_at_ms,updated_at_ms,parked_at_ms,parked_deadline_ms) VALUES ('corrupt','corrupt','workspace-reclaim','hash','track','{}',?,'{\"data\":{\"workspace_reclaim\":{\"execution_id\":null}}}','parked',1,1,1,2)")
            .bind(payload.to_string()).execute(&mut **tx).await.unwrap();
        let unrelated = if damage == Some("relative") {
            std::path::PathBuf::from("../independent-retirement-test")
        } else { root.path().join("unrelated") };
        require_active_workspace_path(&mut tx, &unrelated).await.unwrap();
        if matches!(damage, Some("empty" | "relative")) {
            let path = if damage == Some("empty") { "" } else { ".." };
            assert!(retire_extra_worktree_tx(&mut tx, event, "owner", Path::new(path)).await.unwrap());
            require_active_workspace_path(&mut tx, &unrelated).await.unwrap();
            let rows = retired_workspaces(&mut tx).await.unwrap();
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0].cleanup_reason, "unknown_location");
        }
        if !matches!(damage, Some("path" | "json" | "empty" | "relative")) {
            retired(require_active_workspace_path(&mut tx, &root.path().join("extra")).await);
        }
        tx.rollback().await.unwrap();
    })).await.unwrap();
}

#[tokio::test]
async fn retirement_repository_area_delete_preserves_reservation() {
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let root = tempfile::tempdir().unwrap();
    let track = create(
        &repo,
        "",
        TrackWorkspacePlan::ManagedUnder(root.path().into()),
    )
    .await;
    crate::db::RepoSyncDomainRaw::area_delete(&repo, track.area_id.as_str())
        .await
        .unwrap();
    retired(
        require_active_workspace_path(
            &mut repo.pool().acquire().await.unwrap(),
            Path::new(track.workspace.agent_cwd()),
        )
        .await,
    );
}

#[tokio::test]
async fn retirement_unadmitted_missing_receipt_path_does_not_gate_unrelated_execution() {
    unadmitted_target(Some("path")).await;
}

#[tokio::test]
async fn retirement_unadmitted_invalid_receipt_json_does_not_gate_unrelated_execution() {
    unadmitted_target(Some("json")).await;
}

#[tokio::test]
async fn retirement_close_ignores_unreadable_extra_receipts() {
    logical_write_with_unreadable_receipt("close").await;
}

#[tokio::test]
async fn retirement_track_delete_ignores_unreadable_extra_receipts() {
    logical_write_with_unreadable_receipt("track").await;
}

#[tokio::test]
async fn retirement_area_delete_ignores_unreadable_extra_receipts() {
    logical_write_with_unreadable_receipt("area").await;
}

async fn logical_write_with_unreadable_receipt(operation: &str) {
    use crate::db::{RepoSyncDomainRaw, sqlite::append_decision_event_in_tx};
    use crate::{
        event::{Event, EventScope},
        ids::ActorId,
        model::TrackPatch,
    };
    let repo = SqlxRepo::open("sqlite::memory:").await.unwrap();
    let root = tempfile::tempdir().unwrap();
    for missing_path in [true, false] {
        let track = create(
            &repo,
            "",
            TrackWorkspacePlan::ManagedUnder(root.path().into()),
        )
        .await;
        let owner = track.clone();
        repo.db()
            .writer()
            .with_writer(|session| {
                Box::pin(async move {
                    let mut tx = session.begin_immediate().await.unwrap();
                    let id = append_decision_event_in_tx(
                        &mut tx,
                        &ActorId::KernelDispatcher,
                        &EventScope::Card {
                            card: "card".into(),
                            track: owner.id.clone(),
                            area: owner.area_id,
                        },
                        None,
                        &Event::WorktreeProvisioned {
                            track_id: owner.id,
                            card_id: "card".into(),
                            path: "/unlocated-extra".into(),
                        },
                    )
                    .await
                    .unwrap();
                    let payload = if missing_path { "{}" } else { "invalid-json" };
                    sqlx::query("UPDATE events SET payload=? WHERE id=?")
                        .bind(payload)
                        .bind(id)
                        .execute(&mut **tx)
                        .await
                        .unwrap();
                    tx.commit().await.unwrap();
                })
            })
            .await
            .unwrap();
        match operation {
            "close" => {
                repo.track_update(
                    track.id.as_str(),
                    TrackPatch {
                        closed: Some(true),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
                repo.track_update(
                    track.id.as_str(),
                    TrackPatch {
                        closed: Some(false),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
                require_active_workspace_path(
                    &mut repo.pool().acquire().await.unwrap(),
                    Path::new(track.workspace.agent_cwd()),
                )
                .await
                .unwrap();
            }
            "track" => repo.track_delete(track.id.as_str()).await.unwrap(),
            "area" => repo.area_delete(track.area_id.as_str()).await.unwrap(),
            _ => unreachable!(),
        }
        if operation != "close" {
            retired(
                require_active_workspace_path(
                    &mut repo.pool().acquire().await.unwrap(),
                    Path::new(track.workspace.agent_cwd()),
                )
                .await,
            );
        }
    }
}

#[tokio::test]
async fn retirement_unlocated_empty_path_cannot_reserve_all_execution_roots() {
    unadmitted_target(Some("empty")).await;
}

#[tokio::test]
async fn retirement_unlocated_relative_path_cannot_reserve_unrelated_execution_roots() {
    unadmitted_target(Some("relative")).await;
}
