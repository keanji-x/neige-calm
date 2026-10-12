use std::os::unix::fs::PermissionsExt;

use super::*;

const CANARY_ACCESS: &str = "us_m_CANARY-ACCESS-7f3a9c";
const CANARY_REFRESH: &str = "CANARY-REFRESH-51d0e2";

fn client() -> ClientId {
    ClientId::parse("test-client").unwrap()
}

fn token(expires_at: u64) -> TokenFile {
    TokenFile {
        client_id: "test-client".into(),
        access_token: Secret::new(CANARY_ACCESS),
        refresh_token: Some(Secret::new(CANARY_REFRESH)),
        expires_at,
    }
}

/// A token file exactly as the SDK writes one: `serde_json::to_string_pretty`
/// of `OAuthToken` (`rust/crates/oauth/src/token.rs:103`).
fn sdk_token_file(client_id: &str, refresh: Option<&str>) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "client_id": client_id,
        "access_token": CANARY_ACCESS,
        "refresh_token": refresh,
        "expires_at": 4_102_444_800u64,
    }))
    .unwrap()
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

fn entries(dir: &Path) -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn save_then_load_round_trips_with_mode_0600_and_no_temp_residue() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("oauth");
    let storage = DataDirTokenStorage::open(&dir).unwrap();
    assert_eq!(mode(&dir), 0o700);

    storage.save_checked(&token(1_900_000_000)).unwrap();
    let path = storage.path_for(&client());
    assert_eq!(path, dir.join("test-client.json"));
    assert_eq!(mode(&path), 0o600);
    assert_eq!(entries(&dir), ["test-client.json"]);

    let loaded = storage.load_checked(&client()).unwrap().unwrap();
    assert_eq!(loaded.client_id, "test-client");
    assert_eq!(loaded.access_token.expose(), CANARY_ACCESS);
    assert_eq!(
        loaded.refresh_token.as_ref().unwrap().expose(),
        CANARY_REFRESH
    );
    assert_eq!(loaded.expires_at, 1_900_000_000);
}

#[test]
fn overwrite_replaces_a_wider_file_with_a_0600_one() {
    let root = tempfile::tempdir().unwrap();
    let storage = DataDirTokenStorage::open(root.path()).unwrap();
    let path = storage.path_for(&client());
    fs::write(&path, sdk_token_file("test-client", None)).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

    storage.save_checked(&token(1_900_000_001)).unwrap();
    assert_eq!(mode(&path), 0o600);
    assert_eq!(
        storage.load_checked(&client()).unwrap().unwrap().expires_at,
        1_900_000_001
    );
}

#[test]
fn trait_save_and_load_go_through_the_same_file() {
    let root = tempfile::tempdir().unwrap();
    let storage = DataDirTokenStorage::open(root.path()).unwrap();
    let stored: StoredToken = token(1_900_000_002).into();
    TokenStorage::save(&storage, &stored).unwrap();
    let back = TokenStorage::load(&storage, "test-client").unwrap();
    assert_eq!(back.access_token, CANARY_ACCESS);
    assert_eq!(back.refresh_token.as_deref(), Some(CANARY_REFRESH));
    assert_eq!(back.expires_at, 1_900_000_002);
}

#[test]
fn read_only_save_refuses_and_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let storage = DataDirTokenStorage::read_only(root.path());

    let err = storage.save_checked(&token(1)).unwrap_err();
    assert!(matches!(err, TokenFileError::ReadOnly { .. }), "{err}");
    let err = TokenStorage::save(&storage, &token(1).into()).unwrap_err();
    assert!(err.to_string().contains("read-only"), "{err}");
    assert!(entries(root.path()).is_empty());

    // An existing file is left byte-identical.
    let path = storage.path_for(&client());
    let original = sdk_token_file("test-client", Some(CANARY_REFRESH));
    fs::write(&path, &original).unwrap();
    assert!(TokenStorage::save(&storage, &token(2).into()).is_err());
    assert_eq!(fs::read_to_string(&path).unwrap(), original);
    assert_eq!(entries(root.path()), ["test-client.json"]);
    // Reading still works.
    assert_eq!(
        storage.load_checked(&client()).unwrap().unwrap().expires_at,
        4_102_444_800
    );
}

#[test]
fn read_only_mode_does_not_create_its_directory() {
    let root = tempfile::tempdir().unwrap();
    let dir = root.path().join("absent");
    let storage = DataDirTokenStorage::read_only(&dir);
    assert!(storage.load_checked(&client()).unwrap().is_none());
    assert!(storage.save_checked(&token(1)).is_err());
    assert!(!dir.exists());
}

#[test]
fn missing_is_none_but_malformed_is_a_redacted_error() {
    let root = tempfile::tempdir().unwrap();
    let storage = DataDirTokenStorage::open(root.path()).unwrap();
    assert!(storage.load_checked(&client()).unwrap().is_none());

    // serde would quote this mistyped value in its own message.
    let path = storage.path_for(&client());
    fs::write(
        &path,
        format!(r#"{{"client_id":"test-client","access_token":"x","refresh_token":null,"expires_at":"{CANARY_ACCESS}"}}"#),
    )
    .unwrap();
    let err = storage.load_checked(&client()).unwrap_err();
    assert!(
        matches!(
            err,
            TokenFileError::Parse {
                category: "data",
                ..
            }
        ),
        "{err}"
    );
    assert!(!err.to_string().contains(CANARY_ACCESS), "{err}");
    assert!(!format!("{err:?}").contains(CANARY_ACCESS));
    // The SDK-facing trait has no error channel; it reports none.
    assert!(TokenStorage::load(&storage, "test-client").is_none());

    fs::write(&path, "{not json").unwrap();
    assert!(matches!(
        storage.load_checked(&client()).unwrap_err(),
        TokenFileError::Parse {
            category: "syntax",
            ..
        }
    ));
}

#[test]
fn a_file_for_another_client_id_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let storage = DataDirTokenStorage::open(root.path()).unwrap();
    fs::write(
        storage.path_for(&client()),
        sdk_token_file("other-client", None),
    )
    .unwrap();
    assert!(matches!(
        storage.load_checked(&client()).unwrap_err(),
        TokenFileError::ClientIdMismatch { .. }
    ));
}

#[test]
fn import_reads_an_sdk_written_file_into_storage() {
    let root = tempfile::tempdir().unwrap();
    // The SDK names its file after the client id, without an extension.
    let from = root.path().join("test-client");
    fs::write(&from, sdk_token_file("test-client", Some(CANARY_REFRESH))).unwrap();
    let storage = DataDirTokenStorage::open(root.path().join("oauth")).unwrap();

    let imported = import_token(&from, &client(), &storage).unwrap();
    assert_eq!(
        imported,
        TokenSummary {
            expires_at: 4_102_444_800,
            has_refresh_token: true
        }
    );
    let path = storage.path_for(&client());
    assert_eq!(mode(&path), 0o600);
    let stored = storage.load_checked(&client()).unwrap().unwrap();
    assert_eq!(stored.access_token.expose(), CANARY_ACCESS);
    assert_eq!(stored.refresh_token.unwrap().expose(), CANARY_REFRESH);
    // The stored file is the same JSON shape the SDK reads.
    let shape: serde_json::Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    let mut keys: Vec<_> = shape.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(
        keys,
        ["access_token", "client_id", "expires_at", "refresh_token"]
    );

    // A token without a refresh token (`null`) imports too.
    fs::write(&from, sdk_token_file("test-client", None)).unwrap();
    assert!(
        !import_token(&from, &client(), &storage)
            .unwrap()
            .has_refresh_token
    );
}

#[test]
fn import_refuses_another_client_or_a_read_only_store_and_writes_nothing() {
    let root = tempfile::tempdir().unwrap();
    let from = root.path().join("token");
    fs::write(&from, sdk_token_file("other-client", None)).unwrap();
    let dir = root.path().join("oauth");
    let storage = DataDirTokenStorage::open(&dir).unwrap();
    assert!(matches!(
        import_token(&from, &client(), &storage).unwrap_err(),
        TokenFileError::ClientIdMismatch { .. }
    ));
    assert!(entries(&dir).is_empty());

    fs::write(&from, sdk_token_file("test-client", None)).unwrap();
    let read_only = DataDirTokenStorage::read_only(&dir);
    assert!(matches!(
        import_token(&from, &client(), &read_only).unwrap_err(),
        TokenFileError::ReadOnly { .. }
    ));
    assert!(entries(&dir).is_empty());
}

#[test]
fn formatting_never_shows_a_token() {
    let root = tempfile::tempdir().unwrap();
    let storage = DataDirTokenStorage::open(root.path()).unwrap();
    let file = token(1);
    let imported = TokenSummary {
        expires_at: 1,
        has_refresh_token: true,
    };
    let rendered = [
        format!("{file:?}"),
        format!("{file:#?}"),
        format!("{}", file.access_token),
        format!("{:?}", file.refresh_token),
        format!("{storage:?}"),
        format!("{imported:?}"),
        format!("{:?}", client()),
    ];
    for text in &rendered {
        assert!(!text.contains("CANARY"), "{text}");
    }
    assert!(rendered[0].contains("<redacted>"));
    assert!(file.access_token.has_prefix("us_"));
}

#[test]
fn identifiers_are_validated() {
    for bad in ["", ".hidden", "../x", "a/b", "a b", &"x".repeat(129)] {
        assert!(ClientId::parse(bad).is_err(), "{bad:?}");
    }
    assert!(ClientId::parse("abc-DEF_1.2").is_ok());

    // The registered port is never 0; 60355, the SDK default, is allowed.
    assert!(CallbackPort::new(0).is_none());
    assert_eq!(CallbackPort::new(60355).unwrap().get(), 60355);
}

#[test]
fn summary_shows_expiry_and_refresh_presence_only() {
    let summary = TokenSummary::from(&token(4_102_444_800));
    assert_eq!(
        summary.to_string(),
        "expires_at=4102444800 (2100-01-01T00:00:00+00:00) refresh_token=present"
    );
}

#[tokio::test]
async fn build_oauth_loads_a_valid_token_without_saving_or_a_browser() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("test-client.json");
    fs::write(&path, sdk_token_file("test-client", Some(CANARY_REFRESH))).unwrap();
    let before = fs::read(&path).unwrap();

    let oauth = build_oauth(&client(), DataDirTokenStorage::read_only(root.path()))
        .await
        .unwrap();
    assert_eq!(oauth.access_token().await.unwrap(), CANARY_ACCESS);
    assert!(!format!("{oauth:?}").contains("CANARY"));
    assert_eq!(fs::read(&path).unwrap(), before);
}
