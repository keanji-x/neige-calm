//! Same expected tenure: concurrent admission and a later byte-identical replay.
use super::*;

async fn counts(p: &Pair) -> (i64, i64, i64) {
    sqlx::query_as(
        "SELECT planner_revision,
        (SELECT count(*) FROM track_planner_changes WHERE track_id=tracks.id),
        (SELECT count(*) FROM events WHERE scope_track=tracks.id AND kind='card.updated')
        FROM tracks WHERE id=?1",
    )
    .bind(&p.track)
    .fetch_one(p.fixture.repo.pool())
    .await
    .unwrap()
}

#[tokio::test]
async fn planner_switch_http_concurrent_and_success_replay() {
    let mut p = Pair::new().await;
    let value = body(&p);
    let before = counts(&p).await;
    let make = || {
        Request::post(format!("/api/tracks/{}/planner/switch", p.track))
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::COOKIE, &p.cookie)
            .header("X-Calm-Actor", "user")
            .body(Body::from(value.to_string()))
            .unwrap()
    };
    let first = p.app.clone().oneshot(make());
    let second = p.app.clone().oneshot(make());
    let both = async { tokio::join!(first, second) };
    tokio::pin!(both);
    let (a,b) = tokio::time::timeout(Duration::from_secs(10), async {
        loop { tokio::select! {
            result = &mut both => break result,
            call = request(&mut p.peer) => {
                assert!(matches!(call["method"].as_str(),Some("thread/unsubscribe"|"turn/interrupt"|"config/read")),"{call}");
                send(&mut p.peer,json!({"id":call["id"],"result":{}})).await;
            }
        }}
    }).await.unwrap();
    let mut responses = [decoded(a.unwrap()).await, decoded(b.unwrap()).await];
    responses.sort_by_key(|r| r.0);
    assert_eq!(responses[0].0, StatusCode::OK, "{responses:?}");
    assert_eq!(responses[1].0, StatusCode::CONFLICT, "{responses:?}");
    assert_eq!(responses[1].1["code"], "planner_changed");
    assert_eq!(responses[0].1["planner_revision"], 1);
    let after = counts(&p).await;
    assert_eq!((after.0, after.1), (1, 1));
    assert_eq!(after.2 - before.2, 2, "one pair of CardUpdated events");
    let (status, error) = submit(&mut p, value).await;
    assert_eq!(status, StatusCode::CONFLICT, "{error}");
    assert_eq!(error["code"], "planner_changed");
    assert_eq!(
        counts(&p).await,
        after,
        "replay must not append events or revisions"
    );
    assert_eq!(detail(&p).await["planner_revision"], 1);
    p.close().await;
}
