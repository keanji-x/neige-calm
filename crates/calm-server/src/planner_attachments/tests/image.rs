use super::*;

fn png() -> &'static [u8] {
    b"\x89PNG\r\n\x1a\nimage bytes"
}

fn setup(workspace: &Path, card: &CardId, attachment: &AttachmentId) -> PathBuf {
    let root = workspace.join(NEIGE_DIR).join("attachments");
    let path = bound_file_path(&root, card, attachment);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, png()).unwrap();
    path
}

#[tokio::test]
async fn bound_image_reader_returns_verified_bytes() {
    let workspace = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let path = setup(workspace.path(), &card, &id("01", AttachmentFormat::Png));
    let (format, bytes) = read_bound_image(workspace.path(), &card, &path)
        .await
        .unwrap();
    assert_eq!(format, AttachmentFormat::Png);
    assert_eq!(bytes, png());
}

#[tokio::test]
async fn bound_image_reader_refuses_foreign_path_even_when_the_same_id_exists_here() {
    let workspace = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let attachment = id("01", AttachmentFormat::Png);
    setup(workspace.path(), &card, &attachment);
    let foreign = setup(workspace.path(), &CardId::from("card-b"), &attachment);
    assert!(matches!(
        read_bound_image(workspace.path(), &card, &foreign).await,
        Err(CalmError::BadRequest(_))
    ));
}

#[tokio::test]
async fn bound_image_reader_refuses_staging_and_symlinks() {
    let workspace = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let attachment = id("01", AttachmentFormat::Png);
    let bound = setup(workspace.path(), &card, &attachment);
    let root = workspace.path().join(NEIGE_DIR).join("attachments");
    let staging = staging_path(&root, &card).join(attachment.as_str());
    std::fs::create_dir_all(staging.parent().unwrap()).unwrap();
    std::fs::rename(&bound, &staging).unwrap();
    for path in [&bound, &staging] {
        assert!(matches!(
            read_bound_image(workspace.path(), &card, path).await,
            Err(CalmError::BadRequest(_))
        ));
    }
    std::os::unix::fs::symlink(&staging, &bound).unwrap();
    assert!(matches!(
        read_bound_image(workspace.path(), &card, &bound).await,
        Err(CalmError::BadRequest(_))
    ));
}

#[tokio::test]
async fn bound_image_reader_refuses_oversize_and_replaced_format() {
    let workspace = tempfile::tempdir().unwrap();
    let card = CardId::from("card-a");
    let path = setup(workspace.path(), &card, &id("01", AttachmentFormat::Png));
    std::fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(MAX_ATTACHMENT_BYTES + 1)
        .unwrap();
    assert!(matches!(
        read_bound_image(workspace.path(), &card, &path).await,
        Err(CalmError::BadRequest(_))
    ));
    std::fs::write(&path, b"GIF89areplaced").unwrap();
    assert!(matches!(
        read_bound_image(workspace.path(), &card, &path).await,
        Err(CalmError::BadRequest(_))
    ));
}
