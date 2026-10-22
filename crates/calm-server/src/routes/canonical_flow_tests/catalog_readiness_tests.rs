//! Historical readiness ordering through the authenticated HTTP creation entry.
use super::*;

async fn providers(app: &axum::Router, cookie: &str, query: &str) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("/api/agent-providers{query}"))
                .header(header::COOKIE, cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap()
}

fn claude_status(body: &Value) -> &str {
    body.as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider"] == "claude")
        .unwrap()["status"]
        .as_str()
        .unwrap()
}

/// A historical mint uses the readiness sampled before waiting for the area;
/// a later refresh cannot change that admission decision in either direction.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn historical_creation_samples_readiness_before_the_area_lock() {
    use crate::test_seams::{PausePoint, install_pause_for_test};
    use std::sync::Arc;
    use tokio::sync::Notify;

    for initially_ready in [true, false] {
        let mut fixture = Fixture::new().await;
        let fake = fixture.root.path().join("claude");
        std::fs::create_dir_all(&fake).unwrap();
        std::fs::copy(
            concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/claude_planner_fake/claude.sh"
            ),
            fake.join("claude"),
        )
        .unwrap();
        std::fs::write(fake.join("scenario"), "exit").unwrap();
        std::fs::write(
            fake.join("auth"),
            if initially_ready {
                "logged-in"
            } else {
                "logged-out"
            },
        )
        .unwrap();
        let config = fixture.root.path().join("claude.json");
        std::fs::write(&config, json!({"claude_binary":fake.join("claude"),"claude_version":"2.1.280","config_dir":fixture.root.path().join("claude-config")}).to_string()).unwrap();
        fixture.claude_config = Some(config);
        let (state, app, cookie) = fixture.application().await;
        let area = post(
            &app,
            &cookie,
            "/api/areas",
            "area",
            json!({"name":"readiness","color":"#123456"}),
        )
        .await["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let route = crate::state::RouteState::from_ref(&state);
        let initial = providers(&app, &cookie, "").await;
        assert_eq!(
            claude_status(&initial),
            if initially_ready {
                "ready"
            } else {
                "unavailable"
            }
        );
        let guard = crate::per_card_lock::lock_key(&route.area_delete_locks, &area).await;
        let pause = PausePoint {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
        };
        install_pause_for_test("key-lock-waiting", &area, pause.clone());
        let body = json!({"planner_provider":"claude", "area_id":area,
            "title":"sampled historical mint", "template_id":"small-change",
            "theme":{"fg":[216,219,226],"bg":[15,20,24]}});
        let create = status(&app, &cookie, "/api/tracks", "sampled-readiness", body);
        tokio::pin!(create);
        tokio::time::timeout(Duration::from_secs(30), async {
            tokio::select! {
                response = &mut create => panic!("creation bypassed held area lock: {response:?}"),
                () = pause.entered.notified() => {}
            }
            std::fs::write(
                fake.join("auth"),
                if initially_ready {
                    "logged-out"
                } else {
                    "logged-in"
                },
            )
            .unwrap();
            let refreshed = providers(&app, &cookie, "?refresh=true").await;
            assert_eq!(
                claude_status(&refreshed),
                if initially_ready {
                    "unavailable"
                } else {
                    "ready"
                }
            );
            pause.release.notify_one();
            drop(guard);
            let status = create.await;
            assert_eq!(
                status,
                if initially_ready {
                    StatusCode::CREATED
                } else {
                    StatusCode::BAD_REQUEST
                },
                "readiness admission"
            );
            let count: i64 = sqlx::query_scalar("SELECT count(*) FROM tracks WHERE area_id=?1")
                .bind(&area)
                .fetch_one(fixture.repo.pool())
                .await
                .unwrap();
            assert_eq!(count, i64::from(initially_ready));
        })
        .await
        .expect("historical creation settles after releasing its area lock");
        fixture.shutdown_process(&state).await;
    }
}
