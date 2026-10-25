//! Canonical pre-allocation refusals use the same owned roots as allocation.
use super::*;

#[tokio::test]
async fn c5_u8b_missing_area_preserves_objective_location_fence() {
    let fixture = Fixture::new().await;
    let (state, app, cookie) = fixture.application().await;
    let a = area(&app, &cookie, "deleted-borrower").await;
    assert_eq!(
        http(
            &app,
            &cookie,
            "DELETE",
            &format!("/api/areas/{}", a["id"].as_str().unwrap()),
        )
        .await
        .0,
        StatusCode::NO_CONTENT
    );
    let path = state.objective_checkout_root().join("reserved");
    std::fs::create_dir_all(&path).unwrap();
    let before = counts(&fixture).await;
    for human in [false, true] {
        let mut request = borrowed(&a, "codex", &path, "none");
        if human {
            request["first_message"] = json!(FIRST_BODY);
        }
        let (status, response) = create(
            &app,
            &cookie,
            if human { "owned-human" } else { "owned-empty" },
            request,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{response}");
        assert_eq!(counts(&fixture).await, before);
    }
    fixture.shutdown_process(&state).await;
}
