//! Reads must retain their authority through filesystem and Git owners.
use super::*;
use std::{path::Path, time::Duration};

fn git(root: &Path, args: &[&str]) {
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args([
                "-c",
                "user.name=Fixture",
                "-c",
                "user.email=fixture@example.test",
                "-c",
                "commit.gpgsign=false"
            ])
            .args(args)
            .status()
            .unwrap()
            .success()
    );
}

async fn share(root: &Path) -> (Fixture, String, String) {
    let mut fx = Fixture::new().await;
    let row = fx
        .state
        .raw_repo()
        .track_create(NewTrack {
            area_id: fx.area.clone().into(),
            title: "Filesystem boundary".into(),
            sort: None,
            cwd: root.to_string_lossy().into_owned(),
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
    let id = link["link"]["delegation"]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let cookie = super::tokens::unlock(&fx, &link).await;
    (fx, id, cookie)
}

#[tokio::test]
async fn gitdiff_refuses_outside_head_identity_even_when_worktree_symlink_is_inside() {
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("shared");
    let private = tmp.path().join("private");
    std::fs::create_dir(&shared).unwrap();
    std::fs::create_dir(&private).unwrap();
    std::fs::write(shared.join("note"), "public").unwrap();
    std::fs::write(private.join("secret"), "historical secret").unwrap();
    git(tmp.path(), &["init", "-q"]);
    git(tmp.path(), &["add", "."]);
    git(tmp.path(), &["commit", "-qm", "initial"]);
    std::fs::remove_file(private.join("secret")).unwrap();
    std::os::unix::fs::symlink(shared.join("note"), private.join("secret")).unwrap();
    let (fx, id, cookie) = share(&shared).await;
    let (status, body) = fx
        .request(
            "GET",
            &format!(
                "/api/fs/gitdiff?share_link={id}&path={}&old_path=shared/note",
                shared.join("note").display()
            ),
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["head_text"], "public");
    for old in [
        "private/secret".to_owned(),
        private.join("secret").to_string_lossy().into_owned(),
    ] {
        let (status, body) = fx
            .request(
                "GET",
                &format!(
                    "/api/fs/gitdiff?share_link={id}&path={}&old_path={old}",
                    shared.join("note").display()
                ),
                Some(&cookie),
                Value::Null,
            )
            .await;
        assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    }
}

#[tokio::test]
async fn filesystem_owners_refuse_symlink_swap_even_when_restored_before_publication() {
    swap_routes("share-scope-admitted", false).await;
}

#[tokio::test]
async fn filesystem_owners_refuse_swap_after_canonicalization() {
    swap_routes("share-fs-before-open", false).await;
}

#[tokio::test]
async fn filesystem_roots_stay_bound_across_root_swap() {
    swap_routes("share-scope-admitted", true).await;
}

async fn swap_routes(stage: &str, replace_root: bool) {
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("shared");
    let inside = shared.join("dir");
    let private = tmp.path().join("private");
    let outside = if replace_root {
        private.join("dir")
    } else {
        private.clone()
    };
    std::fs::create_dir_all(&inside).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    for (root, text) in [
        (&inside, "<svg>public</svg>"),
        (&outside, "<svg>secret</svg>"),
    ] {
        std::fs::write(root.join("note.svg"), text).unwrap();
        git(root, &["init", "-q"]);
        git(root, &["add", "note.svg"]);
        git(root, &["commit", "-qm", "initial"]);
    }
    let (fx, id, cookie) = share(&shared).await;
    let mut failures = Vec::new();
    for endpoint in [
        "readfile",
        "readfile-raw",
        "listdir",
        "gitstatus",
        "gitdiff",
    ] {
        let path = if matches!(endpoint, "listdir" | "gitstatus") {
            inside.clone()
        } else {
            inside.join("note.svg")
        };
        let url = format!("/api/fs/{endpoint}?share_link={id}&path={}", path.display());
        let (status, body) = fx.request("GET", &url, Some(&cookie), Value::Null).await;
        assert_eq!(status, StatusCode::OK, "{endpoint}: {body}");
        let before = calm_server::test_seams::PausePoint::default();
        let after = calm_server::test_seams::PausePoint::default();
        let session = cookie.split_once('=').unwrap().1;

        calm_server::test_seams::install_pause_for_test(
            "share-scope-response",
            session,
            after.clone(),
        );
        let key = if stage == "share-scope-admitted" {
            session.to_owned()
        } else {
            path.canonicalize().unwrap().to_string_lossy().into_owned()
        };
        calm_server::test_seams::install_pause_for_test(stage, &key, before.clone());

        let app = fx.app.clone();
        let cookie = cookie.clone();
        let task =
            tokio::spawn(
                async move { request(&app, "GET", &url, Some(&cookie), Value::Null).await },
            );
        tokio::time::timeout(Duration::from_secs(10), before.entered.notified())
            .await
            .unwrap();
        let held = tmp.path().join("held");
        let swapped = if replace_root { &shared } else { &inside };
        let target = if replace_root { &private } else { &outside };
        std::fs::rename(swapped, &held).unwrap();
        std::os::unix::fs::symlink(target, swapped).unwrap();
        before.release.notify_one();
        tokio::time::timeout(Duration::from_secs(10), after.entered.notified())
            .await
            .unwrap();
        std::fs::remove_file(swapped).unwrap();
        std::fs::rename(&held, swapped).unwrap();
        after.release.notify_one();
        let (status, body) = task.await.unwrap();
        if status != StatusCode::FORBIDDEN {
            failures.push(format!("{endpoint}: {status}: {body}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn non_ascii_selector_header_cannot_select_a_valid_query_credential() {
    let tmp = tempfile::tempdir().unwrap();
    let (fx, id, cookie) = share(tmp.path()).await;
    let response = fx
        .app
        .oneshot(
            Request::get(format!("/api/fs/roots?share_link={id}"))
                .header("cookie", cookie)
                .header(
                    "x-calm-share-link",
                    axum::http::HeaderValue::from_bytes(b"\xff").unwrap(),
                )
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn outside_missing_parents_are_forbidden_by_filesystem_owner() {
    let tmp = tempfile::tempdir().unwrap();
    let shared = tmp.path().join("shared");
    std::fs::create_dir(&shared).unwrap();
    let (fx, id, cookie) = share(&shared).await;
    let outside = tmp.path().join("private/missing/note.svg");
    let mut failures = Vec::new();
    for endpoint in [
        "readfile",
        "readfile-raw",
        "listdir",
        "gitstatus",
        "gitdiff",
    ] {
        let (status, body) = fx
            .request(
                "GET",
                &format!(
                    "/api/fs/{endpoint}?share_link={id}&path={}",
                    outside.display()
                ),
                Some(&cookie),
                Value::Null,
            )
            .await;
        if status != StatusCode::FORBIDDEN {
            failures.push(format!("{endpoint}: {status}: {body}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn scoped_file_routes_reject_directories_and_special_files() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("directory");
    let socket = tmp.path().join("socket.svg");
    let fifo = tmp.path().join("fifo.svg");
    std::fs::create_dir(&dir).unwrap();
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    nix::unistd::mkfifo(&fifo, nix::sys::stat::Mode::S_IRUSR).unwrap();
    let (fx, id, cookie) = share(tmp.path()).await;
    let mut failures = Vec::new();
    for endpoint in ["readfile", "readfile-raw"] {
        for path in [tmp.path(), dir.as_path(), socket.as_path(), fifo.as_path()] {
            let (status, body) = fx
                .request(
                    "GET",
                    &format!("/api/fs/{endpoint}?share_link={id}&path={}", path.display()),
                    Some(&cookie),
                    Value::Null,
                )
                .await;
            if status != StatusCode::BAD_REQUEST {
                failures.push(format!("{endpoint} {}: {status}: {body}", path.display()));
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[tokio::test]
async fn scoped_listing_keeps_directory_aliases_only_for_allowed_targets() {
    let tmp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let inside = tmp.path().join("directory");
    std::fs::create_dir(&inside).unwrap();
    std::os::unix::fs::symlink(&inside, tmp.path().join("inside-alias")).unwrap();
    std::os::unix::fs::symlink(outside.path(), tmp.path().join("outside-alias")).unwrap();
    let (fx, id, cookie) = share(tmp.path()).await;
    let (status, body) = fx
        .request(
            "GET",
            &format!(
                "/api/fs/listdir?share_link={id}&path={}",
                tmp.path().display()
            ),
            Some(&cookie),
            Value::Null,
        )
        .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let entries = body["entries"].as_array().unwrap();
    let entry = |name: &str| entries.iter().find(|entry| entry["name"] == name).unwrap();
    assert_eq!(entry("inside-alias")["is_dir"], true);
    assert_eq!(entry("outside-alias")["is_dir"], false);
}
