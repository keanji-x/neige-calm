//! Exercise the Track owner and its existing readers, never a parallel read path.
use super::tests::{answered, route_state_with_workspace_tracks};
use super::*;
use axum::http::StatusCode;
use http_body_util::BodyExt;
use std::os::unix::fs::{PermissionsExt, symlink};

async fn text(state: &RouteState, track: &str, path: &str) -> Result<ReadFileResponse> {
    read_track_workspace_file(
        State(state.clone()),
        RoutePath(track.into()),
        Query(WorkspacePathQuery { path: path.into() }),
    )
    .await
    .map(|Json(file)| file)
}

async fn raw(state: &RouteState, track: &str, path: &str) -> Result<Response> {
    read_track_workspace_file_raw(
        State(state.clone()),
        RoutePath(track.into()),
        Query(WorkspacePathQuery { path: path.into() }),
    )
    .await
}

#[tokio::test]
async fn track_readme_paths_use_persisted_roots_including_trusted_aliases() {
    let workspaces = tempfile::tempdir().unwrap();
    let a = workspaces.path().join("A");
    let b = workspaces.path().join("B");
    let alias = workspaces.path().join("trusted-alias");
    for (root, value) in [(&a, "A"), (&b, "B")] {
        std::fs::create_dir_all(root.join("docs")).unwrap();
        for path in ["README.md", "docs/README.md", ".hidden.md"] {
            std::fs::write(root.join(path), value).unwrap();
        }
    }
    symlink(&a, &alias).unwrap();
    let (state, track_a, track_b) = route_state_with_workspace_tracks(&alias, &b).await;
    for (track, expected) in [(&track_a, "A"), (&track_b, "B")] {
        for path in ["README.md", "docs/README.md", ".hidden.md"] {
            let read = text(&state, track, path).await.unwrap();
            assert_eq!(read.text, expected, "{track}: {path}");
            assert!(!read.truncated);
        }
    }
}

#[tokio::test]
async fn track_invalid_paths_are_rejected_before_root_io() {
    let workspace = tempfile::tempdir().unwrap();
    let absent = workspace.path().join("absent-root");
    let (state, track, _) = route_state_with_workspace_tracks(&absent, &absent).await;
    for path in [
        "/etc/passwd",
        "../outside",
        "a/../../outside",
        "",
        " ",
        ".",
        "bad\0name",
    ] {
        assert_eq!(
            answered(text(&state, &track, path).await.unwrap_err()).await,
            (StatusCode::BAD_REQUEST, "bad_request".into()),
            "text {path:?} must fail before opening the absent root"
        );
        assert_eq!(
            answered(raw(&state, &track, path).await.unwrap_err()).await,
            (StatusCode::BAD_REQUEST, "bad_request".into()),
            "raw {path:?} must fail before opening the absent root"
        );
    }
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn macos_track_rejects_internal_external_leaf_and_parent_symlinks() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join("real")).unwrap();
    std::fs::write(workspace.path().join("real/file.png"), "inside").unwrap();
    std::fs::write(outside.path().join("file.png"), "outside").unwrap();
    for (target, link) in [
        (PathBuf::from("real/file.png"), "internal-leaf.png"),
        (PathBuf::from("real"), "internal-parent"),
        (outside.path().join("file.png"), "external-leaf.png"),
        (outside.path().to_path_buf(), "external-parent"),
    ] {
        symlink(target, workspace.path().join(link)).unwrap();
    }
    let (state, track, _) =
        route_state_with_workspace_tracks(workspace.path(), outside.path()).await;
    for path in [
        "internal-leaf.png",
        "internal-parent/file.png",
        "external-leaf.png",
        "external-parent/file.png",
    ] {
        for error in [
            text(&state, &track, path).await.unwrap_err(),
            raw(&state, &track, path).await.unwrap_err(),
        ] {
            assert!(
                error.to_string().contains("do not allow symlinks"),
                "{path}: {error}"
            );
            assert_eq!(
                answered(error).await,
                (StatusCode::BAD_REQUEST, "bad_request".into())
            );
        }
    }
}

async fn opened_pair(root: &Path) -> (OpenWorkspaceFile, OpenWorkspaceFile) {
    (
        open_track_workspace_file(root, "parent/file.txt")
            .await
            .unwrap(),
        open_track_workspace_file(root, "parent/file.png")
            .await
            .unwrap(),
    )
}

async fn assert_original_bytes(text: OpenWorkspaceFile, raw: OpenWorkspaceFile) {
    assert_eq!(
        read_workspace_file_response(text).await.unwrap().text,
        "original"
    );
    let bytes = read_workspace_file_raw_response(raw)
        .await
        .unwrap()
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes();
    assert_eq!(bytes.as_ref(), b"original");
}

fn seed_parent(root: &Path, bytes: &str) {
    std::fs::create_dir_all(root.join("parent")).unwrap();
    for name in ["file.txt", "file.png"] {
        std::fs::write(root.join("parent").join(name), bytes).unwrap();
    }
}

#[tokio::test]
async fn track_open_fds_survive_parent_rename_and_symlink_replacement() {
    let workspace = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    seed_parent(workspace.path(), "original");
    seed_parent(outside.path(), "replacement");
    let (text, raw) = opened_pair(workspace.path()).await;
    std::fs::rename(
        workspace.path().join("parent"),
        workspace.path().join("moved"),
    )
    .unwrap();
    symlink(
        outside.path().join("parent"),
        workspace.path().join("parent"),
    )
    .unwrap();
    assert_original_bytes(text, raw).await;
}

#[tokio::test]
async fn track_open_fds_survive_root_rename_and_directory_replacement() {
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path().join("root");
    seed_parent(&root, "original");
    let (text, raw) = opened_pair(&root).await;
    std::fs::rename(&root, workspace.path().join("moved-root")).unwrap();
    seed_parent(&root, "replacement");
    assert_original_bytes(text, raw).await;
}

#[tokio::test]
async fn track_reads_reject_directories_sockets_and_denied_files() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::create_dir(workspace.path().join("directory.png")).unwrap();
    let _socket =
        std::os::unix::net::UnixListener::bind(workspace.path().join("socket.png")).unwrap();
    let denied = workspace.path().join("denied.png");
    std::fs::write(&denied, "private").unwrap();
    std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o0)).unwrap();
    let (state, track, _) =
        route_state_with_workspace_tracks(workspace.path(), workspace.path()).await;
    for path in ["directory.png", "socket.png", "denied.png"] {
        let expected = if path == "denied.png" {
            (StatusCode::FORBIDDEN, "forbidden".into())
        } else {
            (StatusCode::BAD_REQUEST, "bad_request".into())
        };
        assert_eq!(
            answered(text(&state, &track, path).await.unwrap_err()).await,
            expected,
            "{path}"
        );
        assert_eq!(
            answered(raw(&state, &track, path).await.unwrap_err()).await,
            expected,
            "{path}"
        );
    }
}

#[tokio::test]
async fn track_readers_keep_utf8_caps_and_raw_extension_contracts() {
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path();
    let mut bytes = vec![b'a'; MAX_READFILE_BYTES as usize - 1];
    bytes.extend_from_slice("é tail".as_bytes());
    std::fs::write(root.join("large.md"), &bytes).unwrap();
    std::fs::write(root.join("binary.md"), [0xff, 0xfe]).unwrap();
    let image = std::fs::File::create(root.join("large.png")).unwrap();
    image.set_len(MAX_READFILE_RAW_BYTES + 1).unwrap();
    let (state, track, _) = route_state_with_workspace_tracks(root, root).await;
    let large = text(&state, &track, "large.md").await.unwrap();
    assert!(large.truncated);
    assert_eq!(large.size, bytes.len() as u64);
    assert_eq!(large.text.len(), MAX_READFILE_BYTES as usize - 1);
    assert!(
        text(&state, &track, "binary.md")
            .await
            .unwrap_err()
            .to_string()
            .contains("binary or non-UTF-8")
    );
    assert!(
        raw(&state, &track, "large.md")
            .await
            .unwrap_err()
            .to_string()
            .contains("unsupported image extension")
    );
    assert!(
        raw(&state, &track, "large.png")
            .await
            .unwrap_err()
            .to_string()
            .contains("100 MiB cap")
    );

    // Growth after fstat is bounded by the same production handle reader.
    image.set_len(0).unwrap();
    let opened = open_track_workspace_file(root, "large.png").await.unwrap();
    image.set_len(MAX_READFILE_RAW_BYTES + 1).unwrap();
    assert!(
        read_workspace_file_raw_response(opened)
            .await
            .unwrap_err()
            .to_string()
            .contains("100 MiB cap")
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn macos_track_read_capability_does_not_enable_attachments_or_writes() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("file.txt"), "readable").unwrap();
    for policy in [
        WorkspaceSymlinks::Refused,
        WorkspaceSymlinks::FollowedInsideRoot,
    ] {
        let error = open_workspace_regular_file(workspace.path(), "file.txt", policy)
            .await
            .unwrap_err();
        assert!(matches!(error, CalmError::Internal(_)));
        let error = open_workspace_directory(workspace.path(), "subdir", policy)
            .await
            .unwrap_err();
        assert!(matches!(error, CalmError::Internal(_)));
    }
    assert!(matches!(
        open_workspace_root_directory(workspace.path())
            .await
            .unwrap_err(),
        CalmError::Internal(_)
    ));
}
