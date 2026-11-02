use super::*;
use calm_truth::db::sqlite::workspace_retirement::require_active_workspace_path;

#[tokio::test]
async fn retirement_provision_requires_target_before_side_effect() {
    let fx = forge_runtime_fixture().await;
    let sentinel = fx.cwd.path().join("must-not-run");
    let mut payload = no_event_payload(
        &fx,
        "unknown-target",
        shell_probe(&format!(
            "touch {}",
            shell_quote(sentinel.to_str().unwrap())
        )),
        "unknown-target",
    );
    payload.event_spec = Some(serde_json::from_value(json!({"event_kind":"worktree.provisioned","fields":{"path":{"json_field":{"path":"/path"}}}})).unwrap());
    assert!(matches!(
        submit_forge(&fx, payload, "unknown-target").await,
        Err(CalmError::BadRequest(_))
    ));
    assert!(!sentinel.exists());
}

#[tokio::test]
async fn retirement_late_provision_receipt_is_reserved_atomically_for_closed_or_missing_owner() {
    for missing in [false, true] {
        let fx = forge_runtime_fixture().await;
        let patch = serde_json::from_value(json!({"closed":true})).unwrap();
        fx.repo.track_update(&fx.track_id, patch).await.unwrap();
        if missing {
            fx.repo.track_delete(&fx.track_id).await.unwrap();
        }
        let path = fx.cwd.path().join("late-extra");
        let scope = EventScope::Card {
            card: "card-1".into(),
            track: fx.track_id.clone().into(),
            area: "area".into(),
        };
        let event = OwnerEvent {
            actor: ActorId::KernelDispatcher,
            scope,
            event: Event::WorktreeProvisioned {
                track_id: fx.track_id.clone().into(),
                card_id: "card-1".into(),
                path: path.to_str().unwrap().into(),
            },
        };
        fx.repo
            .db()
            .writer()
            .with_writer(|session| {
                Box::pin(async move {
                    let mut tx = session.begin_immediate().await.unwrap();
                    ForgeActionAdapter::new()
                        .record_owner_event_tx(&mut tx, event.clone())
                        .await
                        .unwrap();
                    assert!(require_active_workspace_path(&mut tx, &path).await.is_err());
                    tx.rollback().await.unwrap();
                    let mut tx = session.begin_immediate().await.unwrap();
                    require_active_workspace_path(&mut tx, &path).await.unwrap();
                    ForgeActionAdapter::new()
                        .record_owner_event_tx(&mut tx, event)
                        .await
                        .unwrap();
                    tx.commit().await.unwrap();
                    let mut tx = session.begin_immediate().await.unwrap();
                    assert!(require_active_workspace_path(&mut tx, &path).await.is_err());
                    tx.rollback().await.unwrap();
                })
            })
            .await
            .unwrap();
    }
}
