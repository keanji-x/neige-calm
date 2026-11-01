//! Complete persisted question context reaches live and replayed Planner answers.
use super::{resolve_event_observation, resolve_harness_observation};
use crate::event::{AskAnswer, AskDelivery, AskQuestion, Event, EventScope};
use crate::ids::{ActorId, AreaId, CardId, TrackId};

#[tokio::test]
async fn ask_answered_wake_preserves_complete_detail_live_and_catch_up() {
    let repo = crate::db::sqlite::SqlxRepo::open("sqlite::memory:")
        .await
        .unwrap();
    sqlx::raw_sql(
        "INSERT INTO areas(id,name,color,sort,created_at,updated_at) VALUES('c','Area','red',0,1,1);
         INSERT INTO tracks(id,area_id,title,sort,created_at,updated_at) VALUES('w','c','Track',0,1,1);
         INSERT INTO cards(id,track_id,kind,sort,payload,role,deletable,created_at,updated_at)
         VALUES('planner','w','planner',0,'{}','planner',0,1,1);",
    ).execute(repo.pool()).await.unwrap();
    let track = TrackId::from("w");
    let scope = EventScope::Track {
        track: track.clone(),
        area: AreaId::from("c"),
    };
    let mut live = Vec::new();
    for index in 0..2 {
        let detail = format!(
            "KR-{index} frozen standard\n{} LAST CRITERION",
            "criterion ".repeat(300)
        );
        let mut tx = repo.pool().begin().await.unwrap();
        let id = crate::db::sqlite::append_decision_event_in_tx(
            &mut tx,
            &ActorId::AiPlanner(CardId::from("planner")),
            &scope,
            None,
            &Event::AskRequested {
                track_id: track.clone(),
                questions: vec![AskQuestion {
                    title: "Approve this standard?".into(),
                    detail: Some(detail.clone()),
                    options: vec!["Approve".into(), "Decline".into()],
                }],
                delivery: AskDelivery::Wake,
                action: None,
                source_item_id: None,
            },
        )
        .await
        .unwrap();
        let answered = Event::AskAnswered {
            track_id: track.clone(),
            ask_id: id,
            answers: vec![AskAnswer::Option(index)],
        };
        crate::db::sqlite::append_decision_event_in_tx(
            &mut tx,
            &ActorId::User,
            &scope,
            None,
            &answered,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        let observation = resolve_harness_observation(&repo, &track, &answered)
            .await
            .unwrap()
            .unwrap();
        let text = observation.to_turn_text();
        assert!(
            text.contains(&detail),
            "the full frozen detail identifies the answered standard"
        );
        assert!(text.ends_with(if index == 0 { ": Approve" } else { ": Decline" }));
        assert_eq!(
            resolve_event_observation(&repo, &track, &answered, true)
                .await
                .unwrap(),
            Some(observation)
        );
        live.push(text);
    }
    assert_ne!(
        live[0], live[1],
        "identical titles must retain distinct standard context"
    );
    let replayed =
        crate::harness::catch_up::observations_since(&repo, &track, "non-report-only", 0, None)
            .await
            .unwrap();
    assert_eq!(
        replayed
            .iter()
            .map(|(_, observation)| observation.to_turn_text())
            .collect::<Vec<_>>(),
        live
    );
}
