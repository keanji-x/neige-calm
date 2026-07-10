use super::*;
use crate::config::AppConfig;
use crate::installed::InstalledUnit;
use crate::manifest::{
    AppUnit, BinaryUnit, BundleUnit, CalmServerUnit, Compatibility, CompatibilityV1,
    DbMigrationPolicy, FileManifest, FileUnit, ReleaseManifest, ReleaseManifestV2, ReleaseUnit,
    ReleaseUnits, RestartPolicy, UnitName, WebUnit,
};

#[test]
fn upgrade_stage_verifies_hash_and_copies_package() {
    let tmp = test_temp_dir("upgrade-stage");
    let package_dir = make_bundle_package(&tmp);

    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    let result = stage_upgrade(&cfg, &package_dir, PreflightMode::Bundle).expect("stage");

    assert!(result.staged);
    assert!(result.stage_dir.join("manifest.json").is_file());
    assert!(result.stage_dir.join("bin").join("calm-server").is_file());
}

#[test]
fn stage_web_only_reports_no_restart_required() {
    let tmp = test_temp_dir("upgrade-stage-web");
    let package_dir = make_web_package(&tmp);

    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    cfg.upgrade.current_version_file = Some(write_current_version(&tmp));

    let result = stage_upgrade(&cfg, &package_dir, PreflightMode::WebOnly).expect("stage");

    assert!(result.staged);
    assert!(!result.restart_required);
    assert_eq!(
        result.required_action,
        "activate-staged-release-and-refresh-frontend"
    );
}

#[test]
fn stage_server_only_reports_restart_required() {
    let tmp = test_temp_dir("upgrade-stage-server");
    let package_dir = make_server_package(&tmp);

    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    cfg.upgrade.current_version_file = Some(write_current_version(&tmp));

    let result = stage_upgrade(&cfg, &package_dir, PreflightMode::ServerOnly).expect("stage");

    assert!(result.restart_required);
    assert_eq!(
        result.required_action,
        "activate-staged-release-and-restart-service"
    );
}

#[test]
fn stage_v1_app_only_succeeds_without_activation_support() {
    let tmp = test_temp_dir("upgrade-stage-app-only");
    let package_dir = make_app_package(&tmp);

    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");

    let result = stage_upgrade(&cfg, &package_dir, PreflightMode::AppOnly).expect("stage");

    assert!(result.staged);
    assert!(!result.restart_required);
    assert_eq!(result.required_action, "activation-unsupported");
}

#[test]
fn stage_v2_deferred_only_reports_no_restart_required() {
    let tmp = test_temp_dir("upgrade-stage-v2-deferred");
    let package_dir = make_v2_package(
        &tmp,
        "rel-supervisor",
        UnitName::CalmProcSupervisor,
        ReleaseUnit {
            version: "0.2.0".into(),
            binary_sha256: None,
            tree_sha256: None,
            restart_policy: RestartPolicy::DeferUntilFullReboot,
            db_migration_policy: None,
        },
    );
    let mut installed = installed_state_with_unit(
        "rel-current",
        UnitName::CalmProcSupervisor,
        InstalledUnit {
            version: "0.1.0".into(),
            binary_sha256: None,
            tree_sha256: None,
        },
    );
    installed.compatibility = compat_v2();
    write_installed_state(&tmp.join("data"), &installed).expect("write installed state");

    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    cfg.child.data_dir = Some(tmp.join("data"));

    let result = stage_upgrade(&cfg, &package_dir, PreflightMode::ServerOnly).expect("stage");

    assert!(!result.restart_required);
    assert_eq!(
        result.required_action,
        "activate-staged-release-deferred-until-full-reboot"
    );
}

#[test]
fn stage_v2_without_installed_state_is_denied_by_preflight_gate() {
    let tmp = test_temp_dir("upgrade-stage-v2-no-installed");
    let package_dir = make_v2_package(
        &tmp,
        "rel-no-installed",
        UnitName::CalmServer,
        ReleaseUnit {
            version: "0.2.0".into(),
            binary_sha256: None,
            tree_sha256: None,
            restart_policy: RestartPolicy::RestartViaAdminApi,
            db_migration_policy: Some(DbMigrationPolicy::None),
        },
    );
    let manifest = match read_versioned_manifest(&package_dir).expect("read manifest") {
        VersionedReleaseManifest::V2(manifest) => manifest,
        VersionedReleaseManifest::V1(_) => panic!("expected v2 manifest"),
    };
    assert!(
        read_installed_state(&tmp.join("data"))
            .expect("read installed")
            .is_none()
    );
    assert!(matches!(
        preflight::run_preflight_v2(None, &manifest),
        Verdict::Breaking {
            reason: preflight::BreakingReason::NoInstalledState,
            ..
        }
    ));

    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    cfg.child.data_dir = Some(tmp.join("data"));

    let stage = stage_upgrade(&cfg, &package_dir, PreflightMode::ServerOnly)
        .expect("v2 breaking staging succeeds so apply can return a structured rejection");

    assert!(!stage.preflight.allowed);
    assert!(stage.preflight.reason.contains("NoInstalledState"));
    assert_eq!(stage.preflight.required_action, "allow-breaking-upgrade");
}

#[test]
fn sqlite_backup_fails_through_sqlite3_path_for_invalid_db() {
    let tmp = test_temp_dir("sqlite-backup");
    let db = tmp.join("calm.db");
    fs::write(&db, "not a sqlite database").expect("write db");
    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.child.db_url = Some(format!("sqlite://{}?mode=rwc", db.display()));
    cfg.release.backups = tmp.join("backups");

    let err = backup_sqlite_db(&cfg, "rel-1").expect_err("invalid DB backup must fail");

    assert!(
        err.to_string().contains("sqlite3") || err.to_string().contains("online backup failed")
    );
}

#[cfg(unix)]
#[test]
fn web_only_activation_switches_only_web_symlink_without_restart() {
    let tmp = test_temp_dir("activate");
    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    cfg.release.current_server = tmp.join("current-server");
    cfg.release.current_web = tmp.join("current-web");
    cfg.release.previous_server = tmp.join("previous-server");
    cfg.release.previous_web = tmp.join("previous-web");
    let old_server = cfg.release.root.join("staged").join("rel-server-old");
    let old_web = cfg.release.root.join("staged").join("rel-web-old");
    let new = cfg.release.root.join("staged").join("rel-new");
    write_staged_manifest(&old_server, "rel-server-old");
    write_staged_manifest(&old_web, "rel-web-old");
    write_staged_manifest(&new, "rel-new");
    std::os::unix::fs::symlink(&old_server, &cfg.release.current_server)
        .expect("server current symlink");
    std::os::unix::fs::symlink(&old_web, &cfg.release.current_web).expect("web current symlink");
    let preflight = PreflightResult {
        allowed: true,
        mode: "web-only".into(),
        requires_db_backup: false,
        reason: "ok".into(),
        required_action: "none".into(),
        verdict: None,
    };

    let result = activate_staged_release(&cfg, &new, &preflight, "rel-1").expect("activate");

    assert!(result.activated);
    assert!(!result.restart_required);
    assert_eq!(result.changed_symlinks.len(), 1);
    assert_eq!(result.changed_symlinks[0].role, "web");
    assert_eq!(
        fs::read_link(&cfg.release.current_web).expect("read web current"),
        new
    );
    assert_eq!(
        fs::read_link(&cfg.release.previous_web).expect("read web previous"),
        old_web
    );
    assert_eq!(
        fs::read_link(&cfg.release.current_server).expect("read server current"),
        old_server
    );
    assert!(!cfg.release.previous_server.exists());
}

#[cfg(unix)]
#[test]
fn bundle_activation_switches_server_and_web_with_restart() {
    let tmp = test_temp_dir("activate-bundle");
    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    cfg.release.current_server = tmp.join("current-server");
    cfg.release.current_web = tmp.join("current-web");
    cfg.release.previous_server = tmp.join("previous-server");
    cfg.release.previous_web = tmp.join("previous-web");
    let old_server = cfg.release.root.join("staged").join("rel-server-old");
    let old_web = cfg.release.root.join("staged").join("rel-web-old");
    let new = cfg.release.root.join("staged").join("rel-new");
    write_staged_manifest(&old_server, "rel-server-old");
    write_staged_manifest(&old_web, "rel-web-old");
    write_staged_manifest(&new, "rel-new");
    std::os::unix::fs::symlink(&old_server, &cfg.release.current_server)
        .expect("server current symlink");
    std::os::unix::fs::symlink(&old_web, &cfg.release.current_web).expect("web current symlink");
    let preflight = PreflightResult {
        allowed: true,
        mode: "bundle".into(),
        requires_db_backup: false,
        reason: "ok".into(),
        required_action: "none".into(),
        verdict: None,
    };

    let result = activate_staged_release(&cfg, &new, &preflight, "rel-1").expect("activate");

    assert!(result.restart_required);
    assert_eq!(result.changed_symlinks.len(), 2);
    assert_eq!(
        fs::read_link(&cfg.release.current_server).expect("read server current"),
        new
    );
    assert_eq!(
        fs::read_link(&cfg.release.previous_server).expect("read server previous"),
        old_server
    );
    assert_eq!(
        fs::read_link(&cfg.release.current_web).expect("read web current"),
        new
    );
    assert_eq!(
        fs::read_link(&cfg.release.previous_web).expect("read web previous"),
        old_web
    );
}

#[cfg(unix)]
#[test]
fn v2_activation_switches_server_when_neige_app_and_web_change() {
    let tmp = test_temp_dir("activate-v2-neige-app-web");
    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    cfg.release.current_server = tmp.join("current-server");
    cfg.release.current_web = tmp.join("current-web");
    cfg.release.previous_server = tmp.join("previous-server");
    cfg.release.previous_web = tmp.join("previous-web");
    cfg.child.data_dir = Some(tmp.join("data"));

    let old_server = cfg.release.root.join("staged").join("rel-server-old");
    let old_web = cfg.release.root.join("staged").join("rel-web-old");
    write_staged_manifest(&old_server, "rel-server-old");
    write_staged_manifest(&old_web, "rel-web-old");
    std::os::unix::fs::symlink(&old_server, &cfg.release.current_server)
        .expect("server current symlink");
    std::os::unix::fs::symlink(&old_web, &cfg.release.current_web).expect("web current symlink");

    let installed = InstalledState {
        schema_version: 1,
        release_id: "rel-v0".into(),
        product_major: 0,
        compatibility: compat_v2(),
        units: [
            (
                UnitName::CalmServer,
                installed_unit("0.1.0", Some("calm-v0"), None),
            ),
            (
                UnitName::NeigeApp,
                installed_unit("0.1.0", Some("app-v0"), None),
            ),
            (UnitName::Web, installed_unit("0.1.0", None, Some("web-v0"))),
        ]
        .into_iter()
        .collect(),
        installed_at: "2026-05-30T00:00:00Z".into(),
    };
    write_installed_state(&tmp.join("data"), &installed).expect("write installed state");

    let package_dir = make_v2_package_with_units(
        &tmp,
        "rel-v1",
        [
            (
                UnitName::CalmServer,
                release_unit(
                    "0.1.0",
                    Some("calm-v0"),
                    None,
                    RestartPolicy::RestartViaAdminApi,
                    Some(DbMigrationPolicy::None),
                ),
            ),
            (
                UnitName::NeigeApp,
                release_unit(
                    "0.2.0",
                    Some("app-v1"),
                    None,
                    RestartPolicy::DeferUntilFullReboot,
                    None,
                ),
            ),
            (
                UnitName::Web,
                release_unit(
                    "0.2.0",
                    None,
                    Some("web-v1"),
                    RestartPolicy::RefreshFrontend,
                    None,
                ),
            ),
        ],
    );

    let stage = stage_upgrade(&cfg, &package_dir, PreflightMode::Bundle).expect("stage");
    assert_eq!(stage.preflight.mode, "bundle");
    let result = activate_staged_release(&cfg, &stage.stage_dir, &stage.preflight, "rel-v1")
        .expect("activate");

    assert_eq!(result.changed_symlinks.len(), 2);
    assert_eq!(
        fs::read_link(&cfg.release.current_server).expect("read server current"),
        stage.stage_dir
    );
    assert_eq!(
        fs::read_link(&cfg.release.current_web).expect("read web current"),
        stage.stage_dir
    );

    let installed_after = read_installed_state(&tmp.join("data"))
        .expect("read installed")
        .expect("installed state");
    assert_eq!(
        installed_after.units[&UnitName::CalmServer].version,
        "0.1.0"
    );
    assert_eq!(installed_after.units[&UnitName::NeigeApp].version, "0.2.0");
    assert_eq!(installed_after.units[&UnitName::Web].version, "0.2.0");
}

#[cfg(unix)]
#[test]
fn server_only_activation_switches_only_server_symlink_with_restart() {
    let tmp = test_temp_dir("activate-server");
    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    cfg.release.current_server = tmp.join("current-server");
    cfg.release.current_web = tmp.join("current-web");
    cfg.release.previous_server = tmp.join("previous-server");
    cfg.release.previous_web = tmp.join("previous-web");
    let old_server = cfg.release.root.join("staged").join("rel-server-old");
    let old_web = cfg.release.root.join("staged").join("rel-web-old");
    let new = cfg.release.root.join("staged").join("rel-new");
    write_staged_manifest(&old_server, "rel-server-old");
    write_staged_manifest(&old_web, "rel-web-old");
    write_staged_manifest(&new, "rel-new");
    std::os::unix::fs::symlink(&old_server, &cfg.release.current_server)
        .expect("server current symlink");
    std::os::unix::fs::symlink(&old_web, &cfg.release.current_web).expect("web current symlink");
    let preflight = PreflightResult {
        allowed: true,
        mode: "server-only".into(),
        requires_db_backup: false,
        reason: "ok".into(),
        required_action: "none".into(),
        verdict: None,
    };

    let result = activate_staged_release(&cfg, &new, &preflight, "rel-1").expect("activate");

    assert!(result.restart_required);
    assert_eq!(result.changed_symlinks.len(), 1);
    assert_eq!(result.changed_symlinks[0].role, "server");
    assert_eq!(
        fs::read_link(&cfg.release.current_server).expect("read server current"),
        new
    );
    assert_eq!(
        fs::read_link(&cfg.release.previous_server).expect("read server previous"),
        old_server
    );
    assert_eq!(
        fs::read_link(&cfg.release.current_web).expect("read web current"),
        old_web
    );
    assert!(!cfg.release.previous_web.exists());
}

#[cfg(unix)]
#[test]
fn component_activation_rejects_aliased_split_paths() {
    let tmp = test_temp_dir("activate-aliased");
    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    cfg.release.current_server = tmp.join("current");
    cfg.release.current_web = tmp.join("current");
    cfg.release.previous_server = tmp.join("previous-server");
    cfg.release.previous_web = tmp.join("previous-web");
    let new = cfg.release.root.join("staged").join("rel-new");
    write_staged_manifest(&new, "rel-new");

    let err = activate_staged_release(&cfg, &new, &preflight("web-only"), "rel-new")
        .expect_err("aliased current paths must fail");

    assert!(err.to_string().contains("current_server"));
}

#[cfg(unix)]
#[test]
fn bundle_activation_prevalidates_symlink_slots_before_mutation() {
    let tmp = test_temp_dir("activate-bundle-prevalidate");
    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    cfg.release.current_server = tmp.join("current-server");
    cfg.release.current_web = tmp.join("current-web");
    cfg.release.previous_server = tmp.join("previous-server");
    cfg.release.previous_web = tmp.join("previous-web");
    let old_server = cfg.release.root.join("staged").join("rel-server-old");
    let new = cfg.release.root.join("staged").join("rel-new");
    write_staged_manifest(&old_server, "rel-server-old");
    write_staged_manifest(&new, "rel-new");
    std::os::unix::fs::symlink(&old_server, &cfg.release.current_server)
        .expect("server current symlink");
    fs::write(&cfg.release.current_web, "not a symlink").expect("write regular file");

    let err = activate_staged_release(&cfg, &new, &preflight("bundle"), "rel-new")
        .expect_err("regular web current must fail before mutation");

    assert!(err.to_string().contains("exists and is not a symlink"));
    assert_eq!(
        fs::read_link(&cfg.release.current_server).expect("read server current"),
        old_server
    );
    assert!(!cfg.release.previous_server.exists());
    assert!(!activation_metadata_path(&cfg).exists());
}

#[cfg(unix)]
#[test]
fn rollback_uses_last_activation_metadata() {
    let tmp = test_temp_dir("rollback");
    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    cfg.release.current_server = tmp.join("current-server");
    cfg.release.current_web = tmp.join("current-web");
    cfg.release.previous_server = tmp.join("previous-server");
    cfg.release.previous_web = tmp.join("previous-web");
    let old_web = cfg.release.root.join("staged").join("old-web");
    let stale_previous_web = cfg.release.root.join("staged").join("older-web");
    let new = cfg.release.root.join("staged").join("new-web");
    write_staged_manifest(&old_web, "old-web");
    write_staged_manifest(&stale_previous_web, "older-web");
    write_staged_manifest(&new, "new-web");
    std::os::unix::fs::symlink(&old_web, &cfg.release.current_web).expect("web current symlink");
    std::os::unix::fs::symlink(&stale_previous_web, &cfg.release.previous_web)
        .expect("web previous symlink");
    let preflight = PreflightResult {
        allowed: true,
        mode: "web-only".into(),
        requires_db_backup: false,
        reason: "ok".into(),
        required_action: "none".into(),
        verdict: None,
    };
    activate_staged_release(&cfg, &new, &preflight, "new-web").expect("activate");

    let result = rollback_current(&cfg).expect("rollback");

    assert!(result.rolled_back);
    assert!(!result.restart_required);
    assert_eq!(result.mode, "web-only");
    assert_eq!(
        fs::read_link(&cfg.release.current_web).expect("read web current"),
        old_web
    );
    assert_eq!(
        fs::read_link(&cfg.release.previous_web).expect("read web previous"),
        stale_previous_web
    );
    assert!(!activation_metadata_path(&cfg).exists());
}

#[cfg(unix)]
#[test]
fn rollback_rejects_metadata_current_path_tampering() {
    let tmp = test_temp_dir("rollback-tamper-current");
    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    cfg.release.current_server = tmp.join("current-server");
    cfg.release.current_web = tmp.join("current-web");
    cfg.release.previous_server = tmp.join("previous-server");
    cfg.release.previous_web = tmp.join("previous-web");
    let old_web = cfg.release.root.join("staged").join("old-web");
    let new = cfg.release.root.join("staged").join("new-web");
    write_staged_manifest(&old_web, "old-web");
    write_staged_manifest(&new, "new-web");
    std::os::unix::fs::symlink(&old_web, &cfg.release.current_web).expect("web current symlink");
    activate_staged_release(&cfg, &new, &preflight("web-only"), "new-web").expect("activate");

    let metadata_path = activation_metadata_path(&cfg);
    let mut metadata: ActivationMetadata =
        serde_json::from_slice(&fs::read(&metadata_path).expect("read metadata"))
            .expect("parse metadata");
    metadata.changed_symlinks[0].current = tmp.join("evil-current");
    fs::write(
        &metadata_path,
        serde_json::to_vec_pretty(&metadata).expect("serialize metadata"),
    )
    .expect("write metadata");

    let err = rollback_current(&cfg).expect_err("tampered metadata must fail");

    assert!(err.to_string().contains("does not match configured"));
    assert_eq!(
        fs::read_link(&cfg.release.current_web).expect("read web current"),
        new
    );
}

#[cfg(unix)]
#[test]
fn rollback_rejects_metadata_old_previous_outside_staged_root() {
    let tmp = test_temp_dir("rollback-tamper-old-previous");
    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    cfg.release.current_server = tmp.join("current-server");
    cfg.release.current_web = tmp.join("current-web");
    cfg.release.previous_server = tmp.join("previous-server");
    cfg.release.previous_web = tmp.join("previous-web");
    let old_web = cfg.release.root.join("staged").join("old-web");
    let older_web = cfg.release.root.join("staged").join("older-web");
    let new = cfg.release.root.join("staged").join("new-web");
    let outside = tmp.join("outside-release");
    write_staged_manifest(&old_web, "old-web");
    write_staged_manifest(&older_web, "older-web");
    write_staged_manifest(&new, "new-web");
    write_staged_manifest(&outside, "outside-release");
    std::os::unix::fs::symlink(&old_web, &cfg.release.current_web).expect("web current symlink");
    std::os::unix::fs::symlink(&older_web, &cfg.release.previous_web)
        .expect("web previous symlink");
    activate_staged_release(&cfg, &new, &preflight("web-only"), "new-web").expect("activate");

    let metadata_path = activation_metadata_path(&cfg);
    let mut metadata: ActivationMetadata =
        serde_json::from_slice(&fs::read(&metadata_path).expect("read metadata"))
            .expect("parse metadata");
    metadata.changed_symlinks[0].old_previous = Some(outside);
    fs::write(
        &metadata_path,
        serde_json::to_vec_pretty(&metadata).expect("serialize metadata"),
    )
    .expect("write metadata");

    let err = rollback_current(&cfg).expect_err("unsafe old previous must fail");

    assert!(err.to_string().contains("outside staged root"));
    assert_eq!(
        fs::read_link(&cfg.release.current_web).expect("read web current"),
        new
    );
}

#[cfg(unix)]
#[test]
fn rollback_rejects_previous_target_outside_staged_root() {
    let tmp = test_temp_dir("rollback-unsafe");
    let outside = tmp.join("outside-release");
    write_staged_manifest(&outside, "outside-release");
    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    cfg.release.current_server = tmp.join("current-server");
    cfg.release.previous_server = tmp.join("previous-server");
    fs::create_dir_all(cfg.release.root.join("staged")).expect("stage root");
    std::os::unix::fs::symlink(&outside, &cfg.release.previous_server).expect("previous symlink");

    let err = rollback_current(&cfg).expect_err("unsafe previous must fail");

    assert!(err.to_string().contains("outside staged root"));
}

#[cfg(unix)]
#[test]
fn activate_without_current_removes_stale_previous() {
    let tmp = test_temp_dir("activate-clear-previous");
    let stale = tmp.join("stale");
    fs::create_dir_all(&stale).expect("stale dir");
    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    cfg.release.current_web = tmp.join("current-web");
    cfg.release.previous_web = tmp.join("previous-web");
    let new = cfg.release.root.join("staged").join("rel-new");
    write_staged_manifest(&new, "rel-new");
    std::os::unix::fs::symlink(&stale, &cfg.release.previous_web).expect("previous symlink");
    let preflight = PreflightResult {
        allowed: true,
        mode: "web-only".into(),
        requires_db_backup: false,
        reason: "ok".into(),
        required_action: "none".into(),
        verdict: None,
    };

    activate_staged_release(&cfg, &new, &preflight, "rel-new").expect("activate");

    assert_eq!(
        fs::read_link(&cfg.release.current_web).expect("read current"),
        new
    );
    assert!(!cfg.release.previous_web.exists());
}

#[test]
fn upgrade_stage_refuses_bad_hash() {
    let tmp = test_temp_dir("upgrade-bad-hash");
    let package_dir = make_bundle_package(&tmp);
    fs::write(package_dir.join("bin").join("neige"), "tampered").expect("tamper");

    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    let err =
        stage_upgrade(&cfg, &package_dir, PreflightMode::Bundle).expect_err("bad hash must fail");
    assert!(err.to_string().contains("sha256 mismatch"));
}

#[test]
fn upgrade_stage_refuses_non_empty_target() {
    let tmp = test_temp_dir("upgrade-non-empty");
    let package_dir = make_bundle_package(&tmp);

    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    let staged = cfg.release.root.join("staged").join("rel-1");
    fs::create_dir_all(&staged).expect("create staged");
    fs::write(staged.join("existing"), "x").expect("write existing");

    let err = stage_upgrade(&cfg, &package_dir, PreflightMode::Bundle)
        .expect_err("non-empty target must fail");
    assert!(err.to_string().contains("already exists and is not empty"));
}

#[test]
fn upgrade_stage_rejects_manifest_path_escape() {
    let tmp = test_temp_dir("upgrade-path-escape");
    let package_dir = make_bundle_package(&tmp);

    let manifest_path = package_dir.join("manifest.json");
    let mut manifest: ReleaseManifest =
        serde_json::from_slice(&fs::read(&manifest_path).expect("read manifest"))
            .expect("parse manifest");
    manifest.files[0].path = "../outside".into();
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("rewrite manifest");

    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    let err = stage_upgrade(&cfg, &package_dir, PreflightMode::Bundle)
        .expect_err("path escape must fail");
    assert!(err.to_string().contains("unsafe component"));
}

#[test]
fn upgrade_stage_rejects_release_id_traversal() {
    let tmp = test_temp_dir("upgrade-release-id");
    let package_dir = make_bundle_package(&tmp);
    let manifest_path = package_dir.join("manifest.json");
    let mut manifest = read_manifest(&manifest_path);
    manifest.release_id = "../outside".into();
    write_manifest(&manifest_path, &manifest);

    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    let err = stage_upgrade(&cfg, &package_dir, PreflightMode::Bundle)
        .expect_err("unsafe release id must fail");
    assert!(err.to_string().contains("release_id"));
}

#[test]
fn upgrade_stage_rejects_unmanifested_extra_file() {
    let tmp = test_temp_dir("upgrade-extra-file");
    let package_dir = make_bundle_package(&tmp);
    fs::write(package_dir.join("extra"), "not in manifest").expect("write extra file");

    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    let err =
        stage_upgrade(&cfg, &package_dir, PreflightMode::Bundle).expect_err("extra file must fail");
    assert!(err.to_string().contains("unmanifested file extra"));
}

#[cfg(unix)]
#[test]
fn upgrade_stage_rejects_symlink_payload() {
    use std::os::unix::fs::symlink;

    let tmp = test_temp_dir("upgrade-symlink");
    let package_dir = make_bundle_package(&tmp);
    let target = package_dir.join("bin").join("calm-server");
    fs::remove_file(&target).expect("remove regular file");
    symlink("/tmp/outside", &target).expect("create symlink");

    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    let err =
        stage_upgrade(&cfg, &package_dir, PreflightMode::Bundle).expect_err("symlink must fail");
    assert!(err.to_string().contains("symlink"));
}

#[cfg(unix)]
#[test]
fn upgrade_stage_rejects_symlink_stage_target() {
    use std::os::unix::fs::symlink;

    let tmp = test_temp_dir("upgrade-stage-symlink");
    let package_dir = make_bundle_package(&tmp);
    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    let stage_root = cfg.release.root.join("staged");
    fs::create_dir_all(&stage_root).expect("create stage root");
    symlink(tmp.join("elsewhere"), stage_root.join("rel-1")).expect("create stage symlink");

    let err = stage_upgrade(&cfg, &package_dir, PreflightMode::Bundle)
        .expect_err("stage symlink must fail");
    assert!(err.to_string().contains("symlink"));
}

#[test]
fn upgrade_stage_rejects_non_directory_stage_target() {
    let tmp = test_temp_dir("upgrade-stage-file");
    let package_dir = make_bundle_package(&tmp);
    let mut cfg = AppConfig::starter(tmp.join("config.toml"));
    cfg.release.root = tmp.join("releases");
    let stage_root = cfg.release.root.join("staged");
    fs::create_dir_all(&stage_root).expect("create stage root");
    fs::write(stage_root.join("rel-1"), "not a dir").expect("write stage file");

    let err =
        stage_upgrade(&cfg, &package_dir, PreflightMode::Bundle).expect_err("stage file must fail");
    assert!(err.to_string().contains("not a directory"));
}

fn compat() -> CompatibilityV1 {
    CompatibilityV1 {
        api_version: "1".into(),
        sync_event_version: 1,
        mcp_protocol_version: "2025-11-25".into(),
        web_compat_version: 2,
        min_web_compat_version: 2,
    }
}

fn compat_v2() -> Compatibility {
    Compatibility {
        terminal_frame_version: 4,
        terminal_protocol_version: 4,
        api_version: "1".into(),
        sync_event_version: 1,
        mcp_protocol_version: "2024-11-05".into(),
        plugin_mcp_protocol_version: "2025-11-25".into(),
        web_compat_version: 2,
        min_web_compat_version: 2,
        supervisor_control_version: 1,
    }
}

fn installed_state_with_unit(
    release_id: &str,
    unit_name: UnitName,
    unit: InstalledUnit,
) -> InstalledState {
    InstalledState {
        schema_version: 1,
        release_id: release_id.into(),
        product_major: 0,
        compatibility: compat_v2(),
        units: [(unit_name, unit)].into_iter().collect(),
        installed_at: "2026-05-30T00:00:00Z".into(),
    }
}

fn installed_unit(
    version: &str,
    binary_sha256: Option<&str>,
    tree_sha256: Option<&str>,
) -> InstalledUnit {
    InstalledUnit {
        version: version.into(),
        binary_sha256: binary_sha256.map(str::to_string),
        tree_sha256: tree_sha256.map(str::to_string),
    }
}

fn release_unit(
    version: &str,
    binary_sha256: Option<&str>,
    tree_sha256: Option<&str>,
    restart_policy: RestartPolicy,
    db_migration_policy: Option<DbMigrationPolicy>,
) -> ReleaseUnit {
    ReleaseUnit {
        version: version.into(),
        binary_sha256: binary_sha256.map(str::to_string),
        tree_sha256: tree_sha256.map(str::to_string),
        restart_policy,
        db_migration_policy,
    }
}

fn make_v2_package(
    tmp: &Path,
    release_id: &str,
    unit_name: UnitName,
    unit: ReleaseUnit,
) -> PathBuf {
    let package_dir = tmp.join("pkg-v2").join(release_id);
    fs::create_dir_all(package_dir.join("bin")).expect("create package");
    let payload = match unit_name {
        UnitName::Web => "web/index.html",
        _ => "bin/payload",
    };
    let payload_path = package_dir.join(payload);
    if let Some(parent) = payload_path.parent() {
        fs::create_dir_all(parent).expect("create payload parent");
    }
    fs::write(&payload_path, release_id).expect("write payload");
    let (sha256, bytes) = hash_and_measure_file(&payload_path).expect("hash payload");
    let manifest = ReleaseManifestV2 {
        schema_version: 2,
        release_id: release_id.into(),
        product_major: 0,
        compatibility: compat_v2(),
        units: [(unit_name, unit)].into_iter().collect(),
        files: vec![FileManifest {
            path: payload.into(),
            sha256,
            bytes,
            unit: match unit_name {
                UnitName::NeigeApp => FileUnit::App,
                UnitName::Web => FileUnit::Web,
                UnitName::CalmServer => FileUnit::CalmServer,
                UnitName::CalmProcSupervisor
                | UnitName::NeigeCodexBridge
                | UnitName::NeigeMcpStdioShim
                | UnitName::NeigeCli => FileUnit::Bundle,
            },
        }],
    };
    fs::write(
        package_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("write manifest");
    package_dir
}

fn make_v2_package_with_units(
    tmp: &Path,
    release_id: &str,
    units: impl IntoIterator<Item = (UnitName, ReleaseUnit)>,
) -> PathBuf {
    let package_dir = tmp.join("pkg-v2").join(release_id);
    fs::create_dir_all(&package_dir).expect("create package");
    let units: std::collections::BTreeMap<_, _> = units.into_iter().collect();
    let files = units
        .keys()
        .map(|unit_name| {
            let payload = match unit_name {
                UnitName::Web => "web/dist/index.html".to_string(),
                _ => format!("bin/{unit_name:?}"),
            };
            let payload_path = package_dir.join(&payload);
            if let Some(parent) = payload_path.parent() {
                fs::create_dir_all(parent).expect("create payload parent");
            }
            fs::write(&payload_path, format!("{release_id}:{unit_name:?}")).expect("write payload");
            let (sha256, bytes) = hash_and_measure_file(&payload_path).expect("hash payload");
            FileManifest {
                path: payload,
                sha256,
                bytes,
                unit: match unit_name {
                    UnitName::NeigeApp => FileUnit::App,
                    UnitName::Web => FileUnit::Web,
                    UnitName::CalmServer => FileUnit::CalmServer,
                    UnitName::CalmProcSupervisor
                    | UnitName::NeigeCodexBridge
                    | UnitName::NeigeMcpStdioShim
                    | UnitName::NeigeCli => FileUnit::Bundle,
                },
            }
        })
        .collect();
    let manifest = ReleaseManifestV2 {
        schema_version: 2,
        release_id: release_id.into(),
        product_major: 0,
        compatibility: compat_v2(),
        units,
        files,
    };
    fs::write(
        package_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("write manifest");
    package_dir
}

fn make_bundle_package(tmp: &Path) -> PathBuf {
    let package_dir = tmp.join("pkg");
    fs::create_dir_all(package_dir.join("bin")).expect("create bin");
    fs::create_dir_all(package_dir.join("web").join("dist")).expect("create web");
    for name in [
        "calm-server",
        "neige-codex-bridge",
        "neige-mcp-stdio-shim",
        "neige",
    ] {
        fs::write(package_dir.join("bin").join(name), name).expect("write bin");
    }
    fs::write(
        package_dir.join("web").join("dist").join("index.html"),
        "web",
    )
    .expect("write web");

    let binaries = [
        "calm-server",
        "neige-codex-bridge",
        "neige-mcp-stdio-shim",
        "neige",
    ]
    .into_iter()
    .map(|name| BinaryUnit {
        name: name.into(),
        path: format!("bin/{name}"),
    })
    .collect();
    write_legacy_manifest(
        &package_dir,
        ReleaseManifest {
            schema_version: 1,
            release_id: "rel-1".into(),
            units: ReleaseUnits {
                app: None,
                web: Some(WebUnit {
                    version: "web".into(),
                    compatibility: compat(),
                }),
                calm_server: Some(CalmServerUnit {
                    version: "server".into(),
                    compatibility: compat(),
                    db_migration_policy: DbMigrationPolicy::None,
                }),
                bundle: Some(BundleUnit { binaries }),
            },
            files: manifest_files(
                &package_dir,
                &[
                    ("web/dist/index.html", FileUnit::Web),
                    ("bin/calm-server", FileUnit::CalmServer),
                    ("bin/neige-codex-bridge", FileUnit::Bundle),
                    ("bin/neige-mcp-stdio-shim", FileUnit::Bundle),
                    ("bin/neige", FileUnit::Bundle),
                ],
            ),
        },
    );
    package_dir
}

fn make_web_package(tmp: &Path) -> PathBuf {
    let package_dir = tmp.join("pkg-web");
    fs::create_dir_all(package_dir.join("web").join("dist")).expect("create web");
    fs::write(
        package_dir.join("web").join("dist").join("index.html"),
        "web",
    )
    .expect("write web");
    write_legacy_manifest(
        &package_dir,
        ReleaseManifest {
            schema_version: 1,
            release_id: "rel-web".into(),
            units: ReleaseUnits {
                app: None,
                web: Some(WebUnit {
                    version: "web".into(),
                    compatibility: compat(),
                }),
                calm_server: None,
                bundle: None,
            },
            files: manifest_files(&package_dir, &[("web/dist/index.html", FileUnit::Web)]),
        },
    );
    package_dir
}

fn make_server_package(tmp: &Path) -> PathBuf {
    let package_dir = tmp.join("pkg-server");
    fs::create_dir_all(package_dir.join("bin")).expect("create bin");
    for name in [
        "calm-server",
        "neige-codex-bridge",
        "neige-mcp-stdio-shim",
        "neige",
    ] {
        fs::write(package_dir.join("bin").join(name), name).expect("write bin");
    }
    let binaries = [
        "calm-server",
        "neige-codex-bridge",
        "neige-mcp-stdio-shim",
        "neige",
    ]
    .into_iter()
    .map(|name| BinaryUnit {
        name: name.into(),
        path: format!("bin/{name}"),
    })
    .collect();
    write_legacy_manifest(
        &package_dir,
        ReleaseManifest {
            schema_version: 1,
            release_id: "rel-server".into(),
            units: ReleaseUnits {
                app: None,
                web: None,
                calm_server: Some(CalmServerUnit {
                    version: "server".into(),
                    compatibility: compat(),
                    db_migration_policy: DbMigrationPolicy::None,
                }),
                bundle: Some(BundleUnit { binaries }),
            },
            files: manifest_files(
                &package_dir,
                &[
                    ("bin/calm-server", FileUnit::CalmServer),
                    ("bin/neige-codex-bridge", FileUnit::Bundle),
                    ("bin/neige-mcp-stdio-shim", FileUnit::Bundle),
                    ("bin/neige", FileUnit::Bundle),
                ],
            ),
        },
    );
    package_dir
}

fn make_app_package(tmp: &Path) -> PathBuf {
    let package_dir = tmp.join("pkg-app");
    fs::create_dir_all(package_dir.join("bin")).expect("create bin");
    fs::write(package_dir.join("bin").join("neige-app"), "app").expect("write app");
    write_legacy_manifest(
        &package_dir,
        ReleaseManifest {
            schema_version: 1,
            release_id: "rel-app".into(),
            units: ReleaseUnits {
                app: Some(AppUnit {
                    name: "neige-app".into(),
                    version: "app".into(),
                }),
                web: None,
                calm_server: None,
                bundle: None,
            },
            files: manifest_files(&package_dir, &[("bin/neige-app", FileUnit::App)]),
        },
    );
    package_dir
}

fn write_legacy_manifest(package_dir: &Path, mut manifest: ReleaseManifest) {
    manifest.files.sort_by(|a, b| a.path.cmp(&b.path));
    fs::write(
        package_dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).expect("serialize manifest"),
    )
    .expect("write manifest");
}

fn manifest_files(package_dir: &Path, files: &[(&str, FileUnit)]) -> Vec<FileManifest> {
    files
        .iter()
        .map(|(path, unit)| {
            let (sha256, bytes) =
                hash_and_measure_file(&package_dir.join(path)).expect("hash legacy payload");
            FileManifest {
                path: (*path).into(),
                sha256,
                bytes,
                unit: *unit,
            }
        })
        .collect()
}

fn read_manifest(path: &Path) -> ReleaseManifest {
    serde_json::from_slice(&fs::read(path).expect("read manifest")).expect("parse manifest")
}

fn write_manifest(path: &Path, manifest: &ReleaseManifest) {
    fs::write(
        path,
        serde_json::to_vec_pretty(manifest).expect("serialize manifest"),
    )
    .expect("write manifest");
}

fn write_staged_manifest(path: &Path, release_id: &str) {
    fs::create_dir_all(path).expect("create staged release");
    let manifest = ReleaseManifest {
        schema_version: 1,
        release_id: release_id.into(),
        units: Default::default(),
        files: Vec::new(),
    };
    write_manifest(&path.join("manifest.json"), &manifest);
}

fn write_current_version(tmp: &Path) -> PathBuf {
    let path = tmp.join("current-version.json");
    let current = CurrentVersion {
        api_version: "1".into(),
        sync_event_version: 1,
        mcp_protocol_version: "2025-11-25".into(),
        min_web_compat_version: 2,
        web_compat_version: Some(2),
        plugin_mcp_protocol_version: None,
        supervisor_control_version: None,
    };
    fs::write(
        &path,
        serde_json::to_vec_pretty(&current).expect("serialize current version"),
    )
    .expect("write current version");
    path
}

fn preflight(mode: &str) -> PreflightResult {
    PreflightResult {
        allowed: true,
        mode: mode.into(),
        requires_db_backup: false,
        reason: "ok".into(),
        required_action: "none".into(),
        verdict: None,
    }
}

fn test_temp_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("neige-app-{name}-{}", std::process::id()));
    if path.exists() {
        fs::remove_dir_all(&path).expect("remove stale temp dir");
    }
    fs::create_dir_all(&path).expect("create temp dir");
    path
}
