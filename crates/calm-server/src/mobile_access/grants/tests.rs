use super::*;

fn private_state() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let path = temp.path().join("mobile-grants.json");
    (temp, path)
}
fn open(path: &Path) -> GrantStore {
    let store = GrantStore::default();
    store.load(path.into()).unwrap();
    store
}
const ORIGIN: &str = "https://phone.example.ts.net";

#[test]
fn mobile_grants_restore_only_hashes_at_verified_origin_and_keep_authority() {
    let (_temp, path) = private_state();
    let store = open(&path);
    let token = store
        .mint(ORIGIN, "Phone".into(), DeviceGrant::Scan)
        .unwrap();
    assert_eq!(
        store.get(&token).unwrap().authority,
        SessionAuthority::PairedDevice
    );
    let bytes = std::fs::read_to_string(&path).unwrap();
    assert!(!bytes.contains(&token));
    assert!(
        !bytes.contains("expires"),
        "permanent server grants have no expiry field"
    );
    assert!(bytes.contains(&hash(&token)));
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    store.suspend();
    assert!(store.get(&token).is_none());
    let restored = open(&path);
    assert!(
        restored.get(&token).is_none(),
        "boot must await verified enabled origin"
    );
    restored.activate(ORIGIN).unwrap();
    assert_eq!(
        restored.get(&token).unwrap().authority,
        SessionAuthority::PairedDevice
    );
    assert_eq!(restored.get(&token).unwrap().session_id, token);
    assert!(
        restored.get(&hash(&token)).is_none(),
        "a digest is not a bearer token"
    );
    restored
        .activate("https://different.example.ts.net")
        .unwrap();
    assert!(restored.get(&token).is_none());
    let again = open(&path);
    again.activate(ORIGIN).unwrap();
    assert!(
        again.get(&token).is_none(),
        "origin mismatch revocation must persist"
    );
}

#[test]
fn mobile_grants_logout_revoke_and_disable_persist_without_reviving_other_devices() {
    for action in ["logout", "revoke", "disable"] {
        let (_temp, path) = private_state();
        let store = open(&path);
        let first = store
            .mint(ORIGIN, "First".into(), DeviceGrant::Scan)
            .unwrap();
        let second = store
            .mint(ORIGIN, "Second".into(), DeviceGrant::Scan)
            .unwrap();
        match action {
            "logout" => store.remove(&first).unwrap(),
            "revoke" => store
                .revoke(
                    &store
                        .list()
                        .into_iter()
                        .find(|d| d.device_name == "First")
                        .unwrap()
                        .id,
                )
                .unwrap(),
            _ => store.disable().unwrap(),
        }
        let restored = open(&path);
        restored.activate(ORIGIN).unwrap();
        assert!(restored.get(&first).is_none(), "{action}");
        assert_eq!(
            restored.get(&second).is_some(),
            action != "disable",
            "{action}"
        );
        assert_eq!(restored.list().len(), usize::from(action != "disable"));
    }
}

#[test]
fn mobile_grants_refuse_corruption_permissions_symlinks_and_oversized_state() {
    for mode in [
        "corrupt",
        "permissions",
        "symlink",
        "directory",
        "oversize",
        "parent",
    ] {
        let (temp, path) = private_state();
        match mode {
            "corrupt" => {
                std::fs::write(&path, b"{bad json").unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            "permissions" => {
                std::fs::write(&path, b"{}").unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            }
            "symlink" => {
                let target = temp.path().join("other");
                std::fs::write(&target, b"{}").unwrap();
                std::os::unix::fs::symlink(&target, &path).unwrap();
            }
            "directory" => std::fs::create_dir(&path).unwrap(),
            "oversize" => {
                std::fs::write(&path, vec![b' '; MAX_FILE_BYTES as usize + 1]).unwrap();
                std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
            }
            _ => std::fs::set_permissions(temp.path(), std::fs::Permissions::from_mode(0o755))
                .unwrap(),
        }
        let store = GrantStore::default();
        assert!(store.load(path).is_err(), "{mode}");
        assert!(store.list().is_empty(), "{mode}");
    }
}

#[test]
fn mobile_grants_storage_failure_refuses_success_and_suspends_authentication() {
    let (_temp, path) = private_state();
    let store = open(&path);
    let token = store
        .mint(ORIGIN, "Phone".into(), DeviceGrant::Scan)
        .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(store.remove(&token).is_err());
    assert!(store.get(&token).is_none());
    assert!(
        store
            .mint(ORIGIN, "Another".into(), DeviceGrant::Scan)
            .is_err()
    );
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    store.remove(&token).unwrap();
    let restored = open(&path);
    restored.activate(ORIGIN).unwrap();
    assert!(restored.get(&token).is_none());
}
