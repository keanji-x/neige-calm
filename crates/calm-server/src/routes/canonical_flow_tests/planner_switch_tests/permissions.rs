//! Demotion resets independent Assistant approval choice at the switch boundary.
use super::super::conversation_permission_tests::{assert_wire, next_turn, set_mode};
use super::*;

#[tokio::test]
async fn planner_switch_demoted_assistant_issues_never() {
    for mode in ["full", "ask"] {
        let mut p = Pair::new().await;
        set_mode(&p.app, &p.cookie, &p.cards[0], mode).await;
        let receipt = p.switch().await.unwrap();
        // Observe issuance first: a stale permission must reach the real native wire to fail.
        post(
            &p.app,
            &p.cookie,
            &format!("/api/cards/{}/planner/input", p.cards[0]),
            "demoted-input",
            json!({"text":"Independent Assistant turn"}),
        )
        .await;
        let call = next_turn(&mut p.peer).await;
        assert_wire(&call, "never");
        for event in &receipt.events {
            if let crate::event::Event::CardUpdated(card) = event {
                assert_eq!(
                    card.payload["permission_mode"],
                    if card.id.as_str() == p.cards[0] {
                        "never"
                    } else {
                        mode
                    }
                );
            }
        }
        for (card, expected) in [(&p.cards[0], "never"), (&p.cards[1], mode)] {
            let saved = p.fixture.repo.card_get(card).await.unwrap().unwrap();
            assert_eq!(saved.payload["permission_mode"], expected);
            let payload: String = sqlx::query_scalar("SELECT payload FROM events WHERE kind='card.updated' AND json_extract(payload,'$.id')=?1 ORDER BY id DESC LIMIT 1")
                .bind(card).fetch_one(p.fixture.repo.pool()).await.unwrap();
            let payload: Value = serde_json::from_str(&payload).unwrap();
            assert_eq!(payload["payload"]["permission_mode"], expected);
        }
        p.close().await;
    }
}

#[tokio::test]
async fn planner_switch_permission_reset_rolls_back_on_event_failure() {
    let mut p = Pair::new().await;
    set_mode(&p.app, &p.cookie, &p.cards[0], "full").await;
    let before: Vec<(String, String)> =
        sqlx::query_as("SELECT id,payload FROM cards WHERE track_id=?1 ORDER BY id")
            .bind(&p.track)
            .fetch_all(p.fixture.repo.pool())
            .await
            .unwrap();
    let events: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(p.fixture.repo.pool())
        .await
        .unwrap();
    p.fixture
        .repo
        .db()
        .writer()
        .execute(sqlx::query(
            "CREATE TRIGGER reject_switch_event BEFORE INSERT ON events
         WHEN NEW.kind='card.updated' BEGIN SELECT RAISE(ABORT,'fixture event failure'); END",
        ))
        .await
        .unwrap();
    assert!(matches!(
        p.switch().await,
        Err(PlannerSwitchError::Failed(_))
    ));
    p.unchanged().await;
    let after: Vec<(String, String)> =
        sqlx::query_as("SELECT id,payload FROM cards WHERE track_id=?1 ORDER BY id")
            .bind(&p.track)
            .fetch_all(p.fixture.repo.pool())
            .await
            .unwrap();
    assert_eq!(after, before);
    assert_eq!(
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM events")
            .fetch_one(p.fixture.repo.pool())
            .await
            .unwrap(),
        events
    );
    assert_eq!(http::detail(&p).await["planner_revision"], 0);
    p.fixture
        .repo
        .db()
        .writer()
        .execute(sqlx::query("DROP TRIGGER reject_switch_event"))
        .await
        .unwrap();
    p.close().await;
}
