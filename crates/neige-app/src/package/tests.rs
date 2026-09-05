use super::*;
use crate::manifest::{ReleaseManifestV2, RestartPolicy, UnitName};
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn package_directory_contains_v2_manifest_and_hashes() {
    let _env_guard = ENV_LOCK.lock().expect("env lock");
    with_env_removed("NEIGE_PRODUCT_MAJOR", || {
        with_env_removed(
            "NEIGE_DB_MIGRATION_POLICY",
            package_directory_contains_v2_manifest_and_hashes_inner,
        )
    });
}

fn package_directory_contains_v2_manifest_and_hashes_inner() {
    let tmp = test_temp_dir("package-smoke");
    let src = fake_build_output(&tmp);

    let package_dir = build_package(&PackageConfig {
        release_dir: tmp.join("pkg"),
        out: None,
        release_id: "smoke".into(),
        app_bin: Some(src.join("neige-app")),
        web_dist: Some(src.join("web").join("dist")),
        fe_dist: None,
        bins: required_bins(&src),
    })
    .expect("package");

    assert!(package_dir.join("manifest.json").is_file());
    assert!(package_dir.join("bin").join("calm-server").is_file());
    assert!(
        package_dir
            .join("web")
            .join("dist")
            .join("index.html")
            .is_file()
    );

    let manifest: ReleaseManifestV2 = serde_json::from_slice(
        &fs::read(package_dir.join("manifest.json")).expect("read manifest"),
    )
    .expect("parse manifest");
    assert_eq!(manifest.release_id, "smoke");
    assert_eq!(manifest.schema_version, 2);
    // Pins the `product_major()` default (#1209 bumped it 0 -> 1). This
    // assertion is load-bearing only because the whole test body runs
    // inside `with_env_removed("NEIGE_PRODUCT_MAJOR", ..)` above.
    assert_eq!(manifest.product_major, 1);
    assert_eq!(manifest.compatibility.terminal_frame_version, 4);
    assert_eq!(manifest.compatibility.terminal_protocol_version, 4);
    assert_eq!(manifest.compatibility.api_version, "1");
    assert_eq!(manifest.compatibility.sync_event_version, 1);
    assert_eq!(manifest.compatibility.mcp_protocol_version, "2024-11-05");
    assert_eq!(
        manifest.compatibility.plugin_mcp_protocol_version,
        "2025-11-25"
    );
    assert_eq!(manifest.compatibility.web_compat_version, 2);
    assert_eq!(manifest.compatibility.min_web_compat_version, 2);
    assert_eq!(manifest.compatibility.supervisor_control_version, 1);
    assert_eq!(manifest.units.len(), 7);
    assert_eq!(manifest.units[&UnitName::NeigeApp].version, "0.1.0");
    assert_eq!(manifest.units[&UnitName::Web].version, "9.8.7");
    assert_eq!(
        manifest.units[&UnitName::CalmServer].restart_policy,
        RestartPolicy::RestartViaAdminApi
    );
    assert_eq!(
        manifest.units[&UnitName::CalmServer].db_migration_policy,
        Some(DbMigrationPolicy::ForwardOnly)
    );
    assert_eq!(
        manifest.units[&UnitName::CalmProcSupervisor].restart_policy,
        RestartPolicy::DeferUntilFullReboot
    );
    assert_eq!(
        manifest.units[&UnitName::NeigeCodexBridge].restart_policy,
        RestartPolicy::NextSpawn
    );
    assert_eq!(
        manifest.units[&UnitName::NeigeMcpStdioShim].restart_policy,
        RestartPolicy::NextSpawn
    );
    assert_eq!(
        manifest.units[&UnitName::NeigeCli].restart_policy,
        RestartPolicy::NextSpawn
    );
    assert!(
        manifest.units[&UnitName::CalmServer]
            .binary_sha256
            .as_deref()
            .is_some_and(|hash| hash.len() == 64)
    );
    assert!(
        manifest.units[&UnitName::Web]
            .tree_sha256
            .as_deref()
            .is_some_and(|hash| hash.len() == 64)
    );
    assert!(!manifest.files.is_empty());
    assert!(manifest.files.iter().any(|file| {
        file.path == "web/dist/index.html"
            && file.sha256 == "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
    }));
}

#[test]
fn package_uses_env_product_major_override() {
    let _env_guard = ENV_LOCK.lock().expect("env lock");
    with_env_var("NEIGE_PRODUCT_MAJOR", "7", || {
        with_env_removed("NEIGE_DB_MIGRATION_POLICY", || {
            let tmp = test_temp_dir("package-product-major");
            let src = fake_build_output(&tmp);
            let package_dir = build_package(&PackageConfig {
                release_dir: tmp.join("pkg"),
                out: None,
                release_id: "product-major".into(),
                app_bin: Some(src.join("neige-app")),
                web_dist: Some(src.join("web").join("dist")),
                fe_dist: None,
                bins: required_bins(&src),
            })
            .expect("package");
            let manifest: ReleaseManifestV2 = serde_json::from_slice(
                &fs::read(package_dir.join("manifest.json")).expect("read manifest"),
            )
            .expect("parse manifest");
            assert_eq!(manifest.product_major, 7);
        });
    });
}

#[test]
fn calm_server_db_migration_policy_defaults_to_forward_only() {
    let _env_guard = ENV_LOCK.lock().expect("env lock");
    with_env_removed("NEIGE_DB_MIGRATION_POLICY", || {
        let tmp = test_temp_dir("package-db-policy");
        let src = fake_build_output(&tmp);
        let package_dir = build_package(&PackageConfig {
            release_dir: tmp.join("pkg"),
            out: None,
            release_id: "db-policy".into(),
            app_bin: Some(src.join("neige-app")),
            web_dist: Some(src.join("web").join("dist")),
            fe_dist: None,
            bins: required_bins(&src),
        })
        .expect("package");
        let manifest: ReleaseManifestV2 = serde_json::from_slice(
            &fs::read(package_dir.join("manifest.json")).expect("read manifest"),
        )
        .expect("parse manifest");
        assert_eq!(
            manifest.units[&UnitName::CalmServer].db_migration_policy,
            Some(DbMigrationPolicy::ForwardOnly)
        );
    });
}

#[test]
fn parse_named_path_requires_name_and_path() {
    assert!(parse_named_path("calm-server=/tmp/calm-server").is_ok());
    assert!(parse_named_path("calm-server").is_err());
    assert!(parse_named_path("=/tmp/calm-server").is_err());
    assert!(parse_named_path("../outside=/tmp/calm-server").is_err());
    assert!(parse_named_path("nested/name=/tmp/calm-server").is_err());
    assert!(parse_named_path("nested\\name=/tmp/calm-server").is_err());
    assert!(parse_named_path(".=/tmp/calm-server").is_err());
    assert!(parse_named_path("..=/tmp/calm-server").is_err());
    assert!(parse_named_path("bad:name=/tmp/calm-server").is_err());
    assert!(parse_named_path("bad name=/tmp/calm-server").is_err());
}

#[test]
fn package_rejects_unsafe_release_id() {
    let tmp = test_temp_dir("bad-release-id");
    let src = fake_build_output(&tmp);

    let err = build_package(&PackageConfig {
        release_dir: tmp.join("pkg"),
        out: None,
        release_id: "../outside".into(),
        app_bin: Some(src.join("neige-app")),
        web_dist: Some(src.join("web").join("dist")),
        fe_dist: None,
        bins: required_bins(&src),
    })
    .expect_err("unsafe release_id must fail");

    assert!(err.to_string().contains("release_id"));
}

#[test]
fn package_rejects_missing_required_binary() {
    let tmp = test_temp_dir("missing-bin");
    let src = fake_build_output(&tmp);
    let mut bins = required_bins(&src);
    bins.retain(|bin| bin.name != "neige");

    let err = build_package(&PackageConfig {
        release_dir: tmp.join("pkg"),
        out: None,
        release_id: "missing".into(),
        app_bin: Some(src.join("neige-app")),
        web_dist: Some(src.join("web").join("dist")),
        fe_dist: None,
        bins,
    })
    .expect_err("missing bin must be refused");

    assert!(err.to_string().contains("missing required binary neige"));
}

#[test]
fn package_rejects_duplicate_bundle_binary_names() {
    let tmp = test_temp_dir("duplicate-bins");
    let src = fake_build_output(&tmp);
    let mut bins = required_bins(&src);
    bins.push(NamedPath {
        name: "calm-server".into(),
        path: src.join("calm-server"),
    });

    let err = build_package(&PackageConfig {
        release_dir: tmp.join("pkg"),
        out: None,
        release_id: "duplicate".into(),
        app_bin: Some(src.join("neige-app")),
        web_dist: Some(src.join("web").join("dist")),
        fe_dist: None,
        bins,
    })
    .expect_err("duplicate bin path must be refused");

    assert!(err.to_string().contains("duplicate binary name"));
}

fn fake_build_output(tmp: &Path) -> PathBuf {
    let src = tmp.join("src");
    fs::create_dir_all(src.join("web").join("dist")).expect("create source web");
    fs::write(src.join("web").join("dist").join("index.html"), "hello").expect("write web");
    fs::write(
        src.join("web").join("package.json"),
        r#"{"version":"9.8.7"}"#,
    )
    .expect("write package json");
    write_script(
        &src.join("calm-server"),
        r#"case "$1" in
  --version) printf 'calm-server 0.1.0\n'; exit 0 ;;
  --emit-kernel-compatibility-json) cat <<'JSON'
{"terminalFrameVersion":4,"terminalProtocolVersion":4,"apiVersion":"1","syncEventVersion":1,"mcpProtocolVersion":"2024-11-05","pluginMcpProtocolVersion":"2025-11-25","webCompatVersion":2,"minWebCompatVersion":2,"supervisorControlVersion":1}
JSON
    exit 0 ;;
esac
exit 2
"#,
    );
    for (name, version) in [
        ("calm-proc-supervisor", "0.1.0"),
        ("neige-codex-bridge", "0.1.0"),
        ("neige-mcp-stdio-shim", "0.1.0"),
        ("neige", "0.1.0"),
        ("neige-app", "0.1.0"),
    ] {
        write_script(
            &src.join(name),
            &format!(
                r#"if [ "$1" = "--version" ]; then
  printf '{name} {version}\n'
  exit 0
fi
exit 2
"#,
            ),
        );
    }
    src
}

fn required_bins(src: &Path) -> Vec<NamedPath> {
    [
        "calm-server",
        "calm-proc-supervisor",
        "neige-codex-bridge",
        "neige-mcp-stdio-shim",
        "neige",
    ]
    .into_iter()
    .map(|name| NamedPath {
        name: name.into(),
        path: src.join(name),
    })
    .collect()
}

fn write_script(path: &Path, body: &str) {
    fs::write(path, format!("#!/bin/sh\n{body}")).expect("write script");
    let mut permissions = fs::metadata(path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("chmod script");
}

fn with_env_var(key: &str, value: &str, f: impl FnOnce()) {
    let old = std::env::var_os(key);
    unsafe { std::env::set_var(key, value) };
    f();
    restore_env(key, old);
}

fn with_env_removed(key: &str, f: impl FnOnce()) {
    let old = std::env::var_os(key);
    unsafe { std::env::remove_var(key) };
    f();
    restore_env(key, old);
}

fn restore_env(key: &str, value: Option<std::ffi::OsString>) {
    match value {
        Some(value) => unsafe { std::env::set_var(key, value) },
        None => unsafe { std::env::remove_var(key) },
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

#[test]
fn alpha_package_covers_next_assets_in_web_integrity() {
    let tmp = test_temp_dir("alpha-both-frontends");
    let src = fake_build_output(&tmp);
    let fe = src.join("fe-dist");
    fs::create_dir(&fe).unwrap();
    fs::write(fe.join("index.html"), "next-one").unwrap();
    let mut cfg = PackageConfig {
        release_dir: tmp.join("alpha-one"),
        out: None,
        release_id: "0.1.0-alpha.1".into(),
        app_bin: Some(src.join("neige-app")),
        web_dist: Some(src.join("web/dist")),
        fe_dist: Some(fe.clone()),
        bins: required_bins(&src),
    };
    let first = build_package(&cfg).unwrap();
    assert_eq!(
        fs::read_to_string(first.join("web/dist/next/index.html")).unwrap(),
        "next-one"
    );
    let manifest = crate::upgrade::verify_v2_package_integrity(&first).unwrap();
    assert!(
        manifest
            .files
            .iter()
            .any(|f| f.path == "web/dist/next/index.html" && f.unit == FileUnit::Web)
    );
    fs::write(fe.join("index.html"), "next-two").unwrap();
    cfg.release_dir = tmp.join("alpha-two");
    let second = build_package(&cfg).unwrap();
    let changed = crate::upgrade::verify_v2_package_integrity(&second).unwrap();
    assert_ne!(
        manifest.units[&UnitName::Web].tree_sha256,
        changed.units[&UnitName::Web].tree_sha256,
        "a next-only change must change the Web upgrade identity"
    );
    fs::write(second.join("web/dist/next/index.html"), "tampered").unwrap();
    assert!(
        crate::upgrade::verify_v2_package_integrity(&second).is_err(),
        "next assets must be verified by the real upgrade verifier"
    );
}

#[test]
fn alpha_package_rejects_legacy_next_namespace_collision() {
    let tmp = test_temp_dir("alpha-reserved-next");
    let src = fake_build_output(&tmp);
    fs::create_dir(src.join("web/dist/next")).unwrap();
    fs::write(src.join("web/dist/next/index.html"), "legacy-owned").unwrap();
    let cfg = PackageConfig {
        release_dir: tmp.join("package"),
        out: None,
        release_id: "alpha".into(),
        app_bin: Some(src.join("neige-app")),
        web_dist: Some(src.join("web/dist")),
        fe_dist: Some(src.join("web/dist")),
        bins: required_bins(&src),
    };
    assert!(
        build_package(&cfg)
            .unwrap_err()
            .to_string()
            .contains("reserved next/")
    );
}
