//! A completed receipt survives repeated recovery without publishing partial history.
use super::*;
use calm_truth::db::{RepoOutOfDomain, RepoRead};
use calm_truth::session_projection_repo::WorkerSessionProjectionRepo;

#[tokio::test]
async fn canonical_acp_retained_items_wait_for_end_across_repeated_cold_open() {
    let fx = Fixture::with_storage("reply", true).await;
    // The real provider fixture completes, but no live observer persists its events.
    let _events = fx.session.subscribe_events();
    fx.dispatch().await.unwrap();
    fx.wait_idle().await;
    fx.session.shutdown().await.unwrap();
    let receipt = crate::db::sqlite::acp_submission_get(
        fx.owner.repo.pool(),
        &fx.owner.worker,
        fx.submission.as_str(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(
        receipt.state,
        crate::db::sqlite::AcpSubmissionState::Completed
    ));
    let outcome: serde_json::Value =
        serde_json::from_str(receipt.outcome_json.as_deref().unwrap()).unwrap();
    assert_eq!(outcome["status"], "completed");
    let items = outcome["items"].as_array().unwrap();
    assert!(!items.is_empty());
    let original_frame = fx
        .owner
        .repo
        .session_projection_handle_state_by_id(&fx.owner.worker)
        .await
        .unwrap()
        .unwrap();
    let frame: crate::harness::snapshot::CanonicalHarnessSnapshot =
        serde_json::from_value(original_frame.clone()).unwrap();
    assert!(frame.canonical_inputs.active.is_some());
    let before_page = serde_json::to_value(
        fx.owner
            .repo
            .conversation_presentation_page(&fx.owner.card, 0, 500, false)
            .await
            .unwrap(),
    )
    .unwrap();
    assert!(
        !before_page["items"].as_array().unwrap().is_empty(),
        "accepted human stays visible"
    );
    let prompts = fx.prompts();
    assert_eq!(prompts, 1);

    // Each iteration opens a new real repository and a new backend. The second
    // open is after a successful recovery and before ANY END observation.
    for pass in 0..2 {
        let owner = super::super::super::dispatch_fixture::Owner {
            repo: Arc::new(
                crate::db::sqlite::SqlxRepo::open(&fx.owner.db_url)
                    .await
                    .unwrap(),
            ),
            card: fx.owner.card.clone(),
            track: fx.owner.track.clone(),
            worker: fx.owner.worker.clone(),
            thread: fx.owner.thread.clone(),
            db_url: fx.owner.db_url.clone(),
        };
        let (backend, session, _held) = session_for(&owner, &fx.dir);
        backend.recover_retained().await.unwrap();
        backend.recover_retained().await.unwrap();
        let reopened = crate::db::sqlite::SqlxRepo::open(&owner.db_url)
            .await
            .unwrap();
        let rows = reopened
            .transcript_rows_of_thread(&owner.card, &owner.thread)
            .await
            .unwrap();
        assert!(
            rows.iter()
                .all(|row| !matches!(row.method.as_str(), "item/started" | "item/completed")),
            "receipt items must not create raw-only history before END"
        );
        assert_eq!(
            serde_json::to_value(
                reopened
                    .conversation_presentation_page(&owner.card, 0, 500, false)
                    .await
                    .unwrap()
            )
            .unwrap(),
            before_page,
            "recovery must not expose receipt content ahead of END"
        );
        assert_eq!(
            reopened
                .session_projection_handle_state_by_id(&owner.worker)
                .await
                .unwrap()
                .unwrap(),
            original_frame,
            "native completion cannot settle the canonical input"
        );
        let retained = crate::db::sqlite::acp_submission_get(
            reopened.pool(),
            &owner.worker,
            fx.submission.as_str(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(matches!(
            retained.state,
            crate::db::sqlite::AcpSubmissionState::Completed
        ));
        assert_eq!(retained.outcome_json, receipt.outcome_json);
        assert_eq!(retained.input_json, receipt.input_json);
        assert_eq!(retained.kept_cleanup, receipt.kept_cleanup);
        assert_eq!(fx.prompts(), prompts);
        if pass == 0 {
            session.shutdown().await.unwrap();
            continue;
        }
        let end = crate::harness::input_dispatch::complete_once(
            owner.repo.as_ref(),
            &backend,
            &owner.card,
            &owner.worker,
        )
        .await
        .unwrap()
        .expect("recovered receipt qualifies through the real END owner");
        let frame: crate::harness::snapshot::CanonicalHarnessSnapshot =
            serde_json::from_value(end.snapshot).unwrap();
        assert!(frame.idle_for_lifecycle());
        assert!(frame.canonical_inputs.active.is_none());
        let settled = owner
            .repo
            .transcript_rows_of_thread(&owner.card, &owner.thread)
            .await
            .unwrap();
        let page = owner
            .repo
            .conversation_presentation_page(&owner.card, 0, 500, false)
            .await
            .unwrap();
        let native: Vec<_> = page
            .items
            .iter()
            .filter_map(|entry| match &entry.source {
                calm_types::conversation_display::ConversationDisplaySource::Native { item } => {
                    Some(item)
                }
                _ => None,
            })
            .collect();
        // This fixture has only non-user receipt frames; END publishes each exact
        // provider identity and payload once, through the existing presentation owner.
        assert_eq!(native.len(), items.len());
        for item in items {
            let params = &item["params"];
            let id = params["item"]["id"].as_str().unwrap();
            let raw: Vec<_> = settled
                .iter()
                .filter(|row| row.item_uuid.as_deref() == Some(id))
                .collect();
            assert_eq!(raw.len(), 1);
            assert_eq!(raw[0].method, item["method"].as_str().unwrap());
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&raw[0].params).unwrap(),
                *params
            );
            let displayed: Vec<_> = native
                .iter()
                .filter(|row| row.item_uuid.as_deref() == Some(id))
                .collect();
            assert_eq!(displayed.len(), 1);
            assert_eq!(displayed[0].id, raw[0].id);
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&displayed[0].params).unwrap(),
                *params
            );
        }
        let settled_rows = settled;
        let settled_page = serde_json::to_value(&page).unwrap();
        for _ in 0..2 {
            assert!(
                crate::harness::input_dispatch::complete_once(
                    owner.repo.as_ref(),
                    &backend,
                    &owner.card,
                    &owner.worker
                )
                .await
                .unwrap()
                .is_none()
            );
        }
        session.shutdown().await.unwrap();
        let repo = crate::db::sqlite::SqlxRepo::open(&owner.db_url)
            .await
            .unwrap();
        assert_eq!(
            repo.transcript_rows_of_thread(&owner.card, &owner.thread)
                .await
                .unwrap(),
            settled_rows
        );
        assert_eq!(
            serde_json::to_value(
                repo.conversation_presentation_page(&owner.card, 0, 500, false)
                    .await
                    .unwrap()
            )
            .unwrap(),
            settled_page
        );
        let retained = crate::db::sqlite::acp_submission_get(
            repo.pool(),
            &owner.worker,
            fx.submission.as_str(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(retained.outcome_json, receipt.outcome_json);
        assert_eq!(fx.prompts(), prompts);
    }
}
