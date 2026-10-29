use super::*;
use crate::event::{Event, EventScope};
use crate::ids::ActorId;
use calm_types::system_assistant::SystemAssistantSource;

async fn target(state: &AppState, track: &str) -> String {
    let route = crate::state::RouteState::from_ref(state);
    state
        .today_summary_issuer
        .as_ref()
        .unwrap()
        .submit(
            &route,
            track.into(),
            BOOTSTRAP.into(),
            crate::model::new_id(),
            "original body".into(),
        )
        .await
        .unwrap()
        .card_id
}
async fn append(state: &AppState, card: &str, subject: &str) -> (i64, Event) {
    let (card, subject) = (card.to_owned(), subject.to_owned());
    crate::db::write_in_tx_typed(state.repo.as_ref(), move |tx| Box::pin(async move {
        let (track, worker, area): (String, String, String) = sqlx::query_as("SELECT c.track_id,c.session_id,t.area_id FROM cards c JOIN tracks t ON c.track_id=t.id WHERE c.id=?1")
            .bind(&card).fetch_one(&mut **tx).await?;
        let scope = EventScope::Card { card:card.clone().into(),track:track.clone().into(),area:area.into() };
        let event = Event::AssistantInputRequested {card_id:card.into(),track_id:track.into(),original_worker:worker,source:SystemAssistantSource::TodaySummary,subject:subject.clone(),version:1,text:subject};
        let id = crate::db::sqlite::append_decision_event_in_tx(tx, &ActorId::Kernel, &scope, None, &event).await?;
        Ok((id,event))
    })).await.unwrap()
}
async fn deliver(state: &AppState, track: &str, row: &(i64, Event)) {
    state
        .dispatcher
        .catch_up_push(track.into(), row.1.clone(), row.0)
        .await;
}
async fn pending(
    fixture: &Fixture,
    card: &str,
) -> (i64, Vec<calm_types::conversation::AcceptedInputRef>) {
    let (_, frame) = frame(fixture, card).await;
    (
        frame.header.push_watermark,
        frame
            .canonical_inputs
            .pending
            .iter()
            .map(|input| input.accepted_input_ref().unwrap())
            .collect(),
    )
}
#[tokio::test]
async fn live_through_and_positive_cold_tail_keep_exact_target_and_planner_prefixes() {
    let mut fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    state.dispatcher.abort_event_listener_for_test();
    let area = post(
        &app,
        &cookie,
        "/api/areas",
        "area",
        json!({"name":"Prefix","color":"#123456"}),
    )
    .await;
    let mut tracks = vec![];
    for key in ["a", "b"] {
        let track=post(&app,&cookie,"/api/tracks",key,json!({"area_id":area["id"],"title":key,"planner_provider":"codex","first_message":format!("Planner {key}"),"theme":{"fg":[255,255,255],"bg":[0,0,0]}})).await;
        tracks.push(track["id"].as_str().unwrap().to_owned());
    }
    let a = target(&state, &tracks[0]).await;
    let b = target(&state, &tracks[1]).await;
    let original_a = pending(&fixture, &a).await;
    let original_b = pending(&fixture, &b).await;
    let planner = fixture.carrier(&tracks[0]).await;
    let a1 = append(&state, &a, "a1").await;
    let wake = Event::TrackWakeRequested {
        track_id: tracks[0].clone().into(),
        source: "summary-prefix-test".into(),
        key: "interleave".into(),
        text: "prefix Planner interleave".into(),
    };
    let wake_id = crate::db::write_in_tx_typed(state.repo.as_ref(), {
        let (track, area, wake) = (
            tracks[0].clone(),
            area["id"].as_str().unwrap().to_owned(),
            wake.clone(),
        );
        move |tx| {
            Box::pin(async move {
                Ok(crate::db::sqlite::append_decision_event_in_tx(
                    tx,
                    &ActorId::Kernel,
                    &EventScope::Track {
                        track: track.into(),
                        area: area.into(),
                    },
                    None,
                    &wake,
                )
                .await?)
            })
        }
    })
    .await
    .unwrap();
    let b1 = append(&state, &b, "b1").await;
    let a2 = append(&state, &a, "a2").await;
    deliver(&state, &tracks[0], &a1).await;
    let through = pending(&fixture, &a).await;
    assert_eq!(through.0, a1.0);
    assert_eq!(&through.1[..original_a.1.len()], original_a.1);
    assert_eq!(through.1.len(), original_a.1.len() + 1);
    assert_eq!(pending(&fixture, &b).await, original_b);
    assert_eq!(
        fixture.carrier(&tracks[0]).await.2.canonical_inputs,
        planner.2.canonical_inputs
    );
    deliver(&state, &tracks[0], &b1).await;
    assert_eq!(pending(&fixture, &a).await, through);
    assert_eq!(pending(&fixture, &b).await, original_b);
    deliver(&state, &tracks[1], &b1).await;
    state
        .dispatcher
        .catch_up_push(tracks[0].clone().into(), wake, wake_id)
        .await;
    assert_eq!(
        fixture.carrier(&tracks[0]).await.2.header.push_watermark,
        wake_id
    );
    assert_eq!(pending(&fixture, &a).await, through);
    let b_before = pending(&fixture, &b).await;
    assert_eq!(b_before.0, b1.0);
    // The unbroadcast tail is accepted by a fresh production boot on a reopened file.
    fixture.shutdown_process(&state).await;
    state
        .retire_dispatch_and_stop_provider_sweep()
        .await
        .unwrap();
    let repo = crate::state::WorkerState::from_ref(&state).repo;
    repo.sqlite_db().unwrap().close().await;
    drop((state, app, cookie));
    fixture.repo.db().close().await;
    fixture.repo = Arc::new(SqlxRepo::open(&fixture.url).await.unwrap());
    let (state, _app, _cookie) = fixture.application().await;
    let cold = pending(&fixture, &a).await;
    assert_eq!(cold.0, a2.0);
    assert_eq!(&cold.1[..through.1.len()], through.1);
    assert_eq!(cold.1.len(), through.1.len() + 1);
    assert_eq!(pending(&fixture, &b).await, b_before);
    state.dispatcher.reconcile_tick_for_test().await;
    assert_eq!(pending(&fixture, &a).await, cold);
    shutdown(&fixture, &state).await;
}
#[tokio::test]
async fn bad_selected_row_holds_tail_until_the_unaccepted_source_is_repaired() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    state.dispatcher.abort_event_listener_for_test();
    activity(&state, &app, &cookie).await;
    let accepted = summary(&state, &app, &cookie, "first", &body(1)).await;
    let track = accepted["track_id"].as_str().unwrap().to_owned();
    let card = accepted["card_id"].as_str().unwrap().to_owned();
    let before = pending(&fixture, &card).await;
    let bad = append(&state, &card, "bad").await;
    let tail = append(&state, &card, "tail").await;
    sqlx::query("UPDATE events SET payload=json_set(payload,'$.version',2) WHERE id=?1")
        .bind(bad.0)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    deliver(&state, &track, &tail).await;
    assert_eq!(pending(&fixture, &card).await, before);
    state.dispatcher.reconcile_tick_for_test().await;
    assert_eq!(pending(&fixture, &card).await, before);
    assert!(matches!(
        authority::Requests::new(&state, &track)
            .submit("must-not-skip", "body")
            .await,
        Err(crate::error::CalmError::Conflict(_) | crate::error::CalmError::Forbidden(_))
    ));
    assert_eq!(pending(&fixture, &card).await, before);
    let planner: String =
        sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
            .bind(&track)
            .fetch_one(fixture.repo.pool())
            .await
            .unwrap();
    sqlx::query(
        "UPDATE events SET payload=json_set(payload,'$.version',1,'$.card_id',?2) WHERE id=?1",
    )
    .bind(bad.0)
    .bind(planner)
    .execute(fixture.repo.pool())
    .await
    .unwrap();
    deliver(&state, &track, &tail).await;
    assert_eq!(
        pending(&fixture, &card).await,
        before,
        "unconfirmed foreign source must hold the selected prefix"
    );
    sqlx::query("UPDATE events SET payload=json_set(payload,'$.card_id',?2) WHERE id=?1")
        .bind(bad.0)
        .bind(&card)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    sqlx::query("UPDATE events SET payload=json_set(payload,'$.version',1) WHERE id=?1")
        .bind(bad.0)
        .execute(fixture.repo.pool())
        .await
        .unwrap();
    deliver(&state, &track, &tail).await;
    let after = pending(&fixture, &card).await;
    assert_eq!(after.0, tail.0);
    assert_eq!(after.1.len(), before.1.len() + 2);
    shutdown(&fixture, &state).await;
}

// Uncommitted foreign receipts must be visible to the production prefix owner.
#[tokio::test]
#[allow(
    deprecated,
    reason = "allocation uses the production owner legacy DB chain glue"
)]
async fn accepted_foreign_target_uses_the_prefix_writer_snapshot() {
    let fixture = Fixture::new().await;
    let (state, _app, _cookie) = fixture.application().await;
    state.dispatcher.abort_event_listener_for_test();
    let area = fixture
        .repo
        .area_create(
            calm_types::organization::LEGACY_OWNER_USER_ID,
            crate::model::NewArea {
                name: "foreign prefix".into(),
                color: "#123456".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let track = fixture
        .repo
        .track_create(crate::model::NewTrack {
            area_id: area.id.clone(),
            title: "prefix".into(),
            sort: None,
            cwd: "/tmp".into(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: crate::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let original = state
        .today_summary_issuer
        .as_ref()
        .unwrap()
        .submit(
            &crate::state::RouteState::from_ref(&state),
            track.id.clone(),
            "opening".into(),
            "first".into(),
            "body".into(),
        )
        .await
        .unwrap();
    let db = fixture.repo.db();
    let mut tx = crate::db::sqlite::begin_immediate_tx(db).await.unwrap();
    let before = crate::db::sqlite::conversation::current_input_content_carrier_tx(
        &mut tx,
        &original.card_id,
    )
    .await
    .unwrap();
    let before_pending = before.checkpoint().pending.clone();
    // Use allocation, registration and acceptance owners; no receipt or execution
    // capability is fabricated. The foreign receipt intentionally is not committed.
    let foreign = crate::db::sqlite::card_create_with_id_tx(
        &mut tx, crate::model::new_id(), crate::model::NewCard {
            track_id: track.id.clone(), kind: "codex".into(), sort: None, title: None,
            payload: json!({"schemaVersion":1,"harness_profile":"assistant","assistant_provider":"codex"}),
        }, crate::model::CardRole::Assistant, false, state.write().role_cache(),
    ).await.unwrap();
    let floor = calm_truth::db::sqlite::event_inputs::event_creation_floor_tx(&mut tx)
        .await
        .unwrap();
    let worker = crate::model::new_id();
    crate::db::sqlite::session_start_runtime_tx(
        &mut tx,
        crate::session_projection_repo::WorkerSessionInit::managed_assistant(
            worker.clone(),
            foreign.id.to_string(),
            crate::session_projection_repo::AgentProvider::Codex,
            crate::session_projection_repo::WorkerSessionState::Idle,
            None,
            serde_json::to_value(crate::harness::snapshot::CanonicalHarnessSnapshot::dormant(
                floor,
            ))
            .unwrap(),
            crate::model::now_ms(),
        ),
    )
    .await
    .unwrap();
    let source = SystemAssistantSource::TodaySummary;
    crate::db::sqlite::conversation::system_assistant_entry_insert_tx(
        &mut tx,
        foreign.id.as_str(),
        source,
        "foreign opening",
    )
    .await
    .unwrap();
    let foreign_event = Event::AssistantInputRequested {
        card_id: foreign.id.clone(),
        track_id: track.id.clone(),
        original_worker: worker,
        source,
        subject: source.bootstrap_subject().into(),
        version: 1,
        text: "foreign opening".into(),
    };
    let foreign_id = crate::db::sqlite::append_decision_event_in_tx(
        &mut tx,
        &ActorId::Kernel,
        &EventScope::Card {
            card: foreign.id.clone(),
            track: track.id.clone(),
            area: area.id.clone(),
        },
        None,
        &foreign_event,
    )
    .await
    .unwrap();
    let (_, foreign_ack) = crate::db::sqlite::conversation::system_assistant_accept_tx(
        &mut tx,
        foreign.id.as_str(),
        foreign_id,
    )
    .await
    .unwrap();
    assert_eq!(foreign_ack.event_id, foreign_id);
    let tail = Event::AssistantInputRequested {
        card_id: original.card_id.clone().into(),
        track_id: track.id.clone(),
        original_worker: original.original_worker.clone(),
        source,
        subject: "tail".into(),
        version: 1,
        text: "tail body".into(),
    };
    let tail_id = crate::db::sqlite::append_decision_event_in_tx(
        &mut tx,
        &ActorId::Kernel,
        &EventScope::Card {
            card: original.card_id.clone().into(),
            track: track.id,
            area: area.id,
        },
        None,
        &tail,
    )
    .await
    .unwrap();
    let carrier = state
        .dispatcher
        .accept_event_prefix_for_test(&mut tx, &original.card_id, Some((&tail, tail_id)))
        .await
        .unwrap();
    assert_eq!(carrier.snapshot()["push_watermark"], tail_id);
    assert_eq!(
        &carrier.checkpoint().pending[..before_pending.len()],
        before_pending
    );
    assert_eq!(carrier.checkpoint().pending.len(), before_pending.len() + 1);
    let tail_ack = crate::db::sqlite::conversation::system_assistant_receipt_tx(
        &mut tx,
        &original.card_id,
        source,
        "tail",
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(tail_ack.event_id, tail_id);
    assert_ne!(tail_ack.observation_id, foreign_ack.observation_id);
    tx.commit().await.unwrap();
    // Pending reconstruction and its later authorization use the same production path.
    state.dispatcher.reconcile_tick_for_test().await;
    shutdown(&fixture, &state).await;
}
