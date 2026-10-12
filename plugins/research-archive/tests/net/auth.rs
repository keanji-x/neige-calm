//! `build_oauth` with a valid stored token: no network, no save, no browser.

use std::{fs, time::Duration};

use research_archive::auth::{ClientId, DataDirTokenStorage, build_oauth};

use crate::hermetic;

#[test]
fn build_oauth_loads_a_valid_token_without_saving_or_a_browser() {
    hermetic::run(async {
        let proxy = hermetic::pinned();
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("test-client.json");
        let token = serde_json::json!({
            "client_id": "test-client",
            "access_token": "ap_m_CANARY-ACCESS-7f3a9c",
            "refresh_token": "CANARY-REFRESH-51d0e2",
            "expires_at": 4_102_444_800u64,
        });
        fs::write(&path, serde_json::to_string_pretty(&token).unwrap()).unwrap();
        let before = fs::read(&path).unwrap();

        let oauth = build_oauth(
            &ClientId::parse("test-client").unwrap(),
            DataDirTokenStorage::open(root.path()).unwrap(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        assert_eq!(
            oauth.access_token().await.unwrap(),
            "ap_m_CANARY-ACCESS-7f3a9c"
        );
        assert!(!format!("{oauth:?}").contains("CANARY"));
        assert_eq!(fs::read(&path).unwrap(), before);
        assert!(proxy.seen().is_empty());
    });
}
