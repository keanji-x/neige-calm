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
    activity(&app, &cookie).await;
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
