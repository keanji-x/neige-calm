use super::*;

#[tokio::test]
async fn attachment_ancestors_cannot_be_symlinks() {
    for relative in [
        ".neige".to_string(),
        ".neige/attachments".to_string(),
        "card".to_string(),
    ] {
        let b = boot().await;
        let card = b.planner_card.id.to_string();
        let (status, body) =
            upload(&b.app, &card, Some("user"), "image/png", png(b"original")).await;
        assert_eq!(status, StatusCode::CREATED, "{body}");
        let id = body["attachmentId"].as_str().unwrap();
        let path = if relative == "card" {
            b.attachments_dir().join(&card)
        } else {
            b.workspace.join(relative)
        };
        let parked = path.with_file_name("parked");
        std::fs::rename(&path, &parked).unwrap();
        std::os::unix::fs::symlink("parked", &path).unwrap();
        let (status, _, _) = read_back(&b.app, &card, id).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{}", path.display());
        let (status, body) =
            upload(&b.app, &card, Some("user"), "image/png", png(b"refused")).await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    }
}

#[tokio::test]
async fn replacing_attachment_parent_during_upload_keeps_the_held_directory() {
    let b = boot().await;
    let card = b.planner_card.id.to_string();
    let a = StreamingUpload::start(&b.app, &card);
    a.send(png(b"held"));
    let part = wait_for_part(&b.staging()).await;
    let original = b.workspace.join(".neige");
    let parked = b.workspace.join("parked");
    std::fs::rename(&original, &parked).unwrap();
    let victim = b.workspace.join("victim");
    std::fs::create_dir(&victim).unwrap();
    std::os::unix::fs::symlink("victim", &original).unwrap();
    let (status, body) = a.finish().await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["attachmentId"].as_str().unwrap();
    assert_eq!(
        std::fs::read(
            parked
                .join("attachments")
                .join(&card)
                .join("staging")
                .join(id)
        )
        .unwrap(),
        png(b"held")
    );
    assert!(
        !parked
            .join("attachments")
            .join(&card)
            .join("staging")
            .join(part)
            .exists()
    );
    assert!(file_names(&victim).is_empty());
    let (status, _, _) = read_back(&b.app, &card, id).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn attachment_permissions_are_private_and_denials_do_not_publish() {
    use std::os::unix::fs::PermissionsExt;
    let b = boot().await;
    let card = b.planner_card.id.to_string();
    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"private")).await;
    assert_eq!(status, StatusCode::CREATED, "{body}");
    let id = body["attachmentId"].as_str().unwrap();
    let file = b.staging().join(id);
    assert_eq!(
        std::fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    for dir in [
        b.staging(),
        b.bound(),
        b.attachments_dir(),
        b.workspace.join(".neige"),
    ] {
        assert_eq!(
            std::fs::metadata(dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o0)).unwrap();
    assert!(
        std::fs::read(&file).is_err(),
        "requires an unprivileged test process"
    );
    let (status, _, _) = read_back(&b.app, &card, id).await;
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(status, StatusCode::BAD_REQUEST);
    std::fs::set_permissions(b.staging(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let (status, body) = upload(&b.app, &card, Some("user"), "image/png", png(b"denied")).await;
    std::fs::set_permissions(b.staging(), std::fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body}");
    assert_eq!(file_names(&b.staging()), vec![id.to_string()]);
}
