//! The Track raw route serving an HTML page, through its handler; the global raw route and image
//! responses stay as they were.
use super::tests::{answered, route_state_with_workspace_tracks};
use super::*;
use axum::http::{HeaderMap, StatusCode};
use http_body_util::BodyExt;

/// Written out, not read off `HTML_PAGE_CSP`: this is the policy the design froze.
const EXPECTED_CSP: &str = "sandbox allow-scripts; default-src 'none'; script-src 'unsafe-inline'; \
     style-src 'unsafe-inline'; img-src data: blob:; font-src data:; media-src data: blob:; \
     form-action 'none'; base-uri 'none'";

const PAGE: &str = "<!doctype html><title>x</title><script>document.title='ran'</script>";

async fn raw(state: &RouteState, track: &str, path: &str) -> Result<Response> {
    read_track_workspace_file_raw(
        State(state.clone()),
        RoutePath(track.into()),
        Query(WorkspacePathQuery { path: path.into() }),
    )
    .await
}

fn headers_of(headers: &HeaderMap) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = headers
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_string()))
        .collect();
    pairs.sort();
    pairs
}

fn pairs(expected: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = expected
        .iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
    pairs.sort();
    pairs
}

#[tokio::test]
async fn track_html_page_is_served_under_the_script_only_sandbox_policy() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join("docs")).unwrap();
    std::fs::write(workspace.path().join("docs/overview.html"), PAGE).unwrap();
    std::fs::write(workspace.path().join("docs/Legacy.HTM"), PAGE).unwrap();
    let (state, track, _) =
        route_state_with_workspace_tracks(workspace.path(), workspace.path()).await;
    for path in ["docs/overview.html", "docs/Legacy.HTM"] {
        let response = raw(&state, &track, path).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        let (parts, body) = response.into_parts();
        assert_eq!(
            headers_of(&parts.headers),
            pairs(&[
                ("content-type", "text/html; charset=utf-8"),
                ("cache-control", "no-store"),
                ("content-security-policy", EXPECTED_CSP),
                ("x-content-type-options", "nosniff"),
                ("referrer-policy", "no-referrer"),
            ]),
            "{path}"
        );
        assert_eq!(
            &body.collect().await.unwrap().to_bytes()[..],
            PAGE.as_bytes()
        );
    }
}

#[tokio::test]
async fn track_image_headers_are_unchanged() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(
        workspace.path().join("icon.svg"),
        br#"<svg xmlns="http://www.w3.org/2000/svg"></svg>"#,
    )
    .unwrap();
    let (state, track, _) =
        route_state_with_workspace_tracks(workspace.path(), workspace.path()).await;
    let response = raw(&state, &track, "icon.svg").await.unwrap();
    assert_eq!(
        headers_of(response.headers()),
        pairs(&[
            ("content-type", "image/svg+xml"),
            ("cache-control", "no-store"),
            ("content-security-policy", "sandbox"),
            ("x-content-type-options", "nosniff"),
        ])
    );
}

#[tokio::test]
async fn track_html_page_outside_the_workspace_is_refused() {
    let workspaces = tempfile::tempdir().unwrap();
    let root = workspaces.path().join("root");
    std::fs::create_dir(&root).unwrap();
    std::fs::write(workspaces.path().join("outside.html"), PAGE).unwrap();
    std::os::unix::fs::symlink(
        workspaces.path().join("outside.html"),
        root.join("escape.html"),
    )
    .unwrap();
    let (state, track, _) = route_state_with_workspace_tracks(&root, &root).await;
    let outside = workspaces.path().join("outside.html");
    for path in [
        "../outside.html",
        "docs/../../outside.html",
        outside.to_str().unwrap(),
        "escape.html",
    ] {
        assert_eq!(
            answered(raw(&state, &track, path).await.unwrap_err()).await,
            (StatusCode::BAD_REQUEST, "bad_request".into()),
            "{path}"
        );
    }
}

#[tokio::test]
async fn track_html_page_over_the_text_cap_is_refused() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(
        workspace.path().join("at-cap.html"),
        vec![b'a'; MAX_READFILE_BYTES as usize],
    )
    .unwrap();
    std::fs::File::create(workspace.path().join("over-cap.html"))
        .unwrap()
        .set_len(MAX_READFILE_BYTES + 1)
        .unwrap();
    let (state, track, _) =
        route_state_with_workspace_tracks(workspace.path(), workspace.path()).await;
    let at_cap = raw(&state, &track, "at-cap.html").await.unwrap();
    assert_eq!(at_cap.status(), StatusCode::OK);
    assert_eq!(
        at_cap.into_body().collect().await.unwrap().to_bytes().len() as u64,
        MAX_READFILE_BYTES
    );
    let error = raw(&state, &track, "over-cap.html").await.unwrap_err();
    assert!(
        error.to_string().contains("HTML page exceeds 2 MiB cap"),
        "{error}"
    );
    assert_eq!(
        answered(error).await,
        (StatusCode::BAD_REQUEST, "bad_request".into())
    );
}

#[tokio::test]
async fn global_raw_route_still_refuses_html() {
    let workspace = tempfile::tempdir().unwrap();
    let page = workspace.path().join("overview.html");
    std::fs::write(&page, PAGE).unwrap();
    let (state, _, _) = route_state_with_workspace_tracks(workspace.path(), workspace.path()).await;
    let error = readfile_raw(
        State(state),
        host_access_tests::owner_principal().await,
        Query(PathQuery {
            path: page.to_string_lossy().into_owned(),
        }),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains("unsupported image extension"),
        "{error}"
    );
    assert_eq!(
        answered(error).await,
        (StatusCode::BAD_REQUEST, "bad_request".into())
    );
}

/// Every path the `html` block accepts (`test-data/report-html-path-v1.json`) is one this route
/// serves as a page, so a block that validates never frames the route's error body.
#[tokio::test]
async fn every_accepted_html_block_path_is_served_as_a_page() {
    #[derive(serde::Deserialize)]
    struct Cases {
        accept: Vec<String>,
    }
    let cases: Cases = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../test-data/report-html-path-v1.json"
    )))
    .unwrap();
    let workspace = tempfile::tempdir().unwrap();
    for path in &cases.accept {
        assert!(
            calm_types::report_blocks::validate_payload(
                calm_types::report_blocks::KIND_HTML,
                &serde_json::json!({ "path": path })
            )
            .is_ok(),
            "{path:?}: the fixture accept case validates"
        );
        let file = workspace
            .path()
            .join(workspace_relative_path(path).unwrap());
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, PAGE).unwrap();
    }
    let (state, track, _) =
        route_state_with_workspace_tracks(workspace.path(), workspace.path()).await;
    for path in &cases.accept {
        let response = raw(&state, &track, path).await.unwrap();
        assert_eq!(
            response.headers().get("content-type").unwrap(),
            "text/html; charset=utf-8",
            "{path:?}"
        );
    }
}

/// A dotfile named `.html` has no extension: the block refuses it and so does this route.
#[tokio::test]
async fn a_dotfile_named_html_is_not_a_page() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join("docs")).unwrap();
    std::fs::write(workspace.path().join("docs/.html"), PAGE).unwrap();
    let (state, track, _) =
        route_state_with_workspace_tracks(workspace.path(), workspace.path()).await;
    assert!(
        calm_types::report_blocks::validate_payload(
            calm_types::report_blocks::KIND_HTML,
            &serde_json::json!({ "path": "docs/.html" })
        )
        .is_err()
    );
    assert_eq!(
        answered(raw(&state, &track, "docs/.html").await.unwrap_err()).await,
        (StatusCode::BAD_REQUEST, "bad_request".into())
    );
}
