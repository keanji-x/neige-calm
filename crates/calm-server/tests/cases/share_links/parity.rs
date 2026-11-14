//! Production routes consumed by TrackRouteBody and NewTrackPage.
use super::*;

async fn post(
    fx: &Fixture,
    path: &str,
    cookie: &str,
    key: &str,
    body: Value,
) -> (StatusCode, Value) {
    let response = fx
        .app
        .clone()
        .oneshot(
            Request::post(path)
                .header("cookie", cookie)
                .header("Idempotency-Key", key)
                .header("X-Calm-Actor", "user")
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

async fn assert_guest(fx: &Fixture, card: &str, link: &str) {
    let authors: Vec<(String, Option<String>, Option<String>)> = sqlx::query_as(
        "SELECT author_kind,author_user_id,author_share_link_id FROM conversation_messages WHERE card_id=?1"
    ).bind(card).fetch_all(fx.state.raw_repo().sqlite_db().unwrap().read_pool()).await.unwrap();
    assert_eq!(
        authors,
        vec![("link_guest".into(), None, Some(link.into()))]
    );
}

#[tokio::test]
async fn assistant_creation_accepts_guest_and_replays_only_its_author() {
    let temp = tempfile::tempdir().unwrap();
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let state = with_repo(&fresh_state().await, repo.clone())
        .with_workspace_root(temp.path().join("workspaces"))
        .with_shared_codex_appserver(calm_server::shared_codex_appserver::SharedCodexAppServer::new_fake_running_with_pending(repo, None));
    let mut fx = Fixture::with_state(state).await;
    let (status, track) = fx
        .request(
            "POST",
            "/api/tracks",
            Some(&fx.cookie),
            json!({"planner_provider":"codex","theme":{"fg":[255,255,255],"bg":[0,0,0]},"area_id":fx.area,"title":"Execution Track"}),
        )
        .await;
    assert_eq!(status, StatusCode::CREATED, "{track}");
    fx.track = track["id"].as_str().unwrap().into();
    let link = fx.create(true).await;
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = super::tokens::unlock(&fx, &link).await;
    let path = format!("/api/tracks/{}/conversations?share_link={id}", fx.track);
    let body = json!({"provider":"codex","text":"Guest Assistant first input"});
    let (status, created) = post(&fx, &path, &cookie, "guest-assistant", body.clone()).await;
    assert_eq!(status, StatusCode::CREATED, "{created}");
    let card = created["id"].as_str().unwrap().to_owned();
    assert_guest(&fx, &card, id).await;
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let turns = fx.state.shared_codex_appserver.started_turns_for_test();
            if turns.iter().any(|(_, items)| items.iter().any(|item|
                matches!(item, provider::InputItem::Text { text } if text.contains("Guest Assistant first input"))
            )) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("guest first input must reach provider execution");

    let replay = post(&fx, &path, &cookie, "guest-assistant", body.clone()).await;
    assert_eq!(replay.0, status);
    assert_eq!(replay.1["id"], created["id"]);
    assert_eq!(replay.1["initialInput"], created["initialInput"]);
    let other = fx.create(true).await;
    let other_id = other["link"]["delegation"]["id"].as_str().unwrap();
    let other_cookie = super::tokens::unlock(&fx, &other).await;
    for (path, cookie) in [
        (
            format!(
                "/api/tracks/{}/conversations?share_link={other_id}",
                fx.track
            ),
            other_cookie,
        ),
        (
            format!("/api/tracks/{}/conversations", fx.track),
            fx.cookie.clone(),
        ),
    ] {
        assert_eq!(
            post(&fx, &path, &cookie, "guest-assistant", body.clone())
                .await
                .0,
            StatusCode::CONFLICT
        );
    }
    let mut side = body;
    side["side"] = json!({"source_card_id":card,"context":"excerpt"});
    assert_eq!(
        post(&fx, &path, &cookie, "guest-side", side).await.0,
        StatusCode::FORBIDDEN
    );
    let sessions: Vec<String> = sqlx::query_scalar("SELECT id FROM worker_sessions")
        .fetch_all(fx.state.raw_repo().sqlite_db().unwrap().read_pool())
        .await
        .unwrap();
    for session in sessions {
        if let Some(handle) = fx.state.harness.remove(&session) {
            let _ = handle.shutdown().await;
        }
    }
}

#[tokio::test]
async fn area_creation_supports_both_routes_and_optional_guest_first_input() {
    let fx = Fixture::new().await;
    let link = fx.create(false).await;
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = super::tokens::unlock(&fx, &link).await;
    for (index, route) in [
        "/api/tracks".into(),
        format!("/api/areas/{}/tracks", fx.area),
    ]
    .into_iter()
    .enumerate()
    {
        let path = format!("{route}?share_link={id}");
        for first in [None, Some("Guest Planner first input")] {
            let body = json!({"planner_provider":"codex","theme":{"fg":[255,255,255],"bg":[0,0,0]},"area_id":fx.area,"title":"Guest track","first_message":first});
            let key = format!("area-{index}-{}", first.is_some());
            let (status, created) = post(&fx, &path, &cookie, &key, body.clone()).await;
            assert_eq!(status, StatusCode::CREATED, "{created}");
            let track = created["id"].as_str().unwrap();
            if first.is_some() {
                let planner: String =
                    sqlx::query_scalar("SELECT id FROM cards WHERE track_id=?1 AND role='planner'")
                        .bind(track)
                        .fetch_one(fx.state.raw_repo().sqlite_db().unwrap().read_pool())
                        .await
                        .unwrap();
                assert_guest(&fx, &planner, id).await;
                assert_eq!(
                    post(&fx, &route, &fx.cookie, &key, body.clone()).await.0,
                    StatusCode::CONFLICT
                );
                let other = fx.create(false).await;
                let other_id = other["link"]["delegation"]["id"].as_str().unwrap();
                let other_cookie = super::tokens::unlock(&fx, &other).await;
                assert_eq!(
                    post(
                        &fx,
                        &format!("{route}?share_link={other_id}"),
                        &other_cookie,
                        &key,
                        body.clone()
                    )
                    .await
                    .0,
                    StatusCode::CONFLICT
                );
            }
            assert_eq!(
                post(&fx, &path, &cookie, &key, body).await.0,
                StatusCode::CREATED
            );
        }
    }
    for extra in [
        json!({"cwd":"/tmp"}),
        json!({"admin":true}),
        json!({"allow_cross_area_cwd":{"area_id":fx.area,"folder_id":1}}),
        json!({"area_id":"foreign"}),
        json!({"fork_report_from":"outside"}),
    ] {
        let mut body = json!({"planner_provider":"codex","theme":{"fg":[255,255,255],"bg":[0,0,0]},"area_id":fx.area,"title":"forbidden"});
        body.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert_eq!(
            post(
                &fx,
                &format!("/api/tracks?share_link={id}"),
                &cookie,
                "forbidden",
                body
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
    let track_link = fx.create(true).await;
    let tid = track_link["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = super::tokens::unlock(&fx, &track_link).await;
    assert_eq!(
        post(
            &fx,
            &format!("/api/tracks?share_link={tid}"),
            &cookie,
            "track-cannot-create",
            json!({"planner_provider":"codex","theme":{"fg":[255,255,255],"bg":[0,0,0]},"area_id":fx.area,"title":"forbidden"})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn filesystem_routes_bound_main_and_rename_paths_to_agent_cwd() {
    let mut fx = Fixture::new().await;
    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let shared = temp.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    std::fs::write(temp.path().join("private-name"), "private").unwrap();
    std::fs::write(outside.path().join("secret"), "private").unwrap();
    std::fs::write(shared.join("note"), "shared").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "shared/note"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.test",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "-qm",
            "initial",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .arg("-C")
                .arg(temp.path())
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    let row = fx
        .state
        .raw_repo()
        .track_create(NewTrack {
            area_id: fx.area.clone().into(),
            title: "Filesystem".into(),
            sort: None,
            cwd: shared.to_string_lossy().into_owned(),
            template_id: None,
            plugin_scope: None,
            template_input: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    fx.track = row.id.to_string();
    let link = fx.create(true).await;
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = super::tokens::unlock(&fx, &link).await;
    let (status, roots) = fx
        .request(
            "GET",
            &format!("/api/fs/roots?share_link={id}"),
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        roots,
        json!([shared.canonicalize().unwrap().to_string_lossy()])
    );
    for (endpoint, name) in [("listdir", ""), ("readfile", "note"), ("gitdiff", "note")] {
        for (root, expected) in [
            (shared.as_path(), StatusCode::OK),
            (outside.path(), StatusCode::FORBIDDEN),
        ] {
            let path = root.join(name);
            let (status, body) = fx
                .request(
                    "GET",
                    &format!("/api/fs/{endpoint}?share_link={id}&path={}", path.display()),
                    Some(&cookie),
                    Value::Null,
                )
                .await;
            assert_eq!(status, expected, "{endpoint}: {body}");
        }
    }
    let (status, changes) = fx
        .request(
            "GET",
            &format!(
                "/api/fs/gitstatus?share_link={id}&path={}",
                shared.display()
            ),
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!changes.to_string().contains("private-name"), "{changes}");
    for old in [
        outside.path().join("secret"),
        temp.path()
            .join("..")
            .join(outside.path().file_name().unwrap())
            .join("secret"),
    ] {
        assert_eq!(
            fx.request(
                "GET",
                &format!(
                    "/api/fs/gitdiff?share_link={id}&path={}&old_path={}",
                    shared.join("note").display(),
                    old.display()
                ),
                Some(&cookie),
                Value::Null
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
    }
}

#[tokio::test]
async fn scoped_planner_input_keeps_guest_author_and_receipt() {
    let fx = Fixture::new().await;
    let (card, _) = super::fixtures::canonical(&fx).await;
    let link = fx.create(true).await;
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = super::tokens::unlock(&fx, &link).await;
    let path = format!("/api/cards/{card}/planner/input?share_link={id}");
    let body = json!({"text":"Scoped guest input"});
    let accepted = post(&fx, &path, &cookie, "scoped-planner", body.clone()).await;
    assert_eq!(accepted.0, StatusCode::OK, "{:?}", accepted.1);
    assert_eq!(
        post(&fx, &path, &cookie, "scoped-planner", body).await,
        accepted
    );
    let author: (String, Option<String>, Option<String>) = sqlx::query_as(
        "SELECT author_kind,author_user_id,author_share_link_id FROM conversation_messages WHERE id=?1"
    ).bind(accepted.1["entry_id"].as_str().unwrap())
        .fetch_one(fx.state.raw_repo().sqlite_db().unwrap().read_pool()).await.unwrap();
    assert_eq!(author, ("link_guest".into(), None, Some(id.into())));
}

#[tokio::test]
async fn sibling_card_terminal_and_foreign_area_are_forbidden() {
    use calm_server::model::{NewCard, NewTerminal};
    let fx = Fixture::new().await;
    let sibling = super::fixtures::track(&fx, &fx.area).await;
    let card = fx
        .state
        .raw_repo()
        .card_create(NewCard {
            track_id: sibling.into(),
            title: None,
            kind: "terminal".into(),
            sort: None,
            payload: json!({"terminal_presentation":{"source":"unrecorded","in_workbench":false}}),
        })
        .await
        .unwrap();
    let terminal = fx
        .state
        .raw_repo()
        .terminal_create(NewTerminal {
            card_id: card.id.clone(),
            program: "/bin/true".into(),
            cwd: "/tmp".into(),
            env: json!({}),
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    let area = fx
        .state
        .raw_repo()
        .area_create(
            OWNER_USER_ID,
            NewArea {
                name: "Other".into(),
                color: "#000".into(),
                sort: None,
            },
        )
        .await
        .unwrap();
    let link = fx.create(true).await;
    let id = link["link"]["delegation"]["id"].as_str().unwrap();
    let cookie = super::tokens::unlock(&fx, &link).await;
    for path in [
        format!("/api/cards/{}/terminal", card.id),
        format!("/api/terminals/{}", terminal.id),
        format!("/api/areas/{}/folders", area.id),
        format!("/api/areas/{}/tracks", area.id),
    ] {
        assert_eq!(
            fx.request(
                "GET",
                &format!("{path}?share_link={id}"),
                Some(&cookie),
                Value::Null
            )
            .await
            .0,
            StatusCode::FORBIDDEN,
            "{path}"
        );
    }
}
