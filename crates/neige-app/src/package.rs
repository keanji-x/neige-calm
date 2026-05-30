use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use anyhow::{Context, anyhow};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::identity::parse_version_output;
use crate::manifest::{
    Compatibility, CompatibilityV1, DbMigrationPolicy, FileManifest, FileUnit, ReleaseManifestV2,
    ReleaseUnit, RestartPolicy, UnitName,
};

#[derive(Debug, Clone)]
pub(crate) struct NamedPath {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub(crate) struct PackageConfig {
    pub release_dir: PathBuf,
    pub out: Option<PathBuf>,
    pub release_id: String,
    pub app_version: Option<String>,
    pub app_bin: Option<PathBuf>,
    pub web_dist: Option<PathBuf>,
    pub web_version: Option<String>,
    pub calm_server_version: Option<String>,
    pub db_migration_policy: DbMigrationPolicy,
    pub compatibility: CompatibilityV1,
    pub bins: Vec<NamedPath>,
}

pub(crate) fn build_package(cfg: &PackageConfig) -> anyhow::Result<PathBuf> {
    validate_release_id(&cfg.release_id).map_err(|err| anyhow!("{err}"))?;
    let package_dir = resolve_package_dir(&cfg.release_dir, cfg.out.as_deref())?;
    ensure_fresh_dir(&package_dir)?;

    let mut output_paths = HashSet::new();
    let mut units = BTreeMap::new();

    let app_hash = copy_file_with_hash(
        cfg.app_bin
            .as_deref()
            .ok_or_else(|| anyhow!("missing required binary neige-app"))?,
        &package_dir.join("bin").join("neige-app"),
        "bin/neige-app",
        FileUnit::App,
        &mut output_paths,
    )?;
    units.insert(
        UnitName::NeigeApp,
        ReleaseUnit {
            version: version_from_binary(
                cfg.app_bin
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing required binary neige-app"))?,
            )?,
            binary_sha256: Some(app_hash.sha256.clone()),
            tree_sha256: None,
            restart_policy: RestartPolicy::DeferUntilFullReboot,
            db_migration_policy: None,
        },
    );

    let mut files = vec![app_hash];
    copy_dir_with_hashes(
        cfg.web_dist
            .as_deref()
            .ok_or_else(|| anyhow!("missing required web/dist"))?,
        &package_dir.join("web").join("dist"),
        "web/dist",
        FileUnit::Web,
        &mut files,
        &mut output_paths,
    )?;
    units.insert(
        UnitName::Web,
        ReleaseUnit {
            version: web_package_version(
                cfg.web_dist
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing required web/dist"))?,
            )?,
            binary_sha256: None,
            tree_sha256: Some(tree_sha256(
                cfg.web_dist
                    .as_deref()
                    .ok_or_else(|| anyhow!("missing required web/dist"))?,
            )?),
            restart_policy: RestartPolicy::RefreshFrontend,
            db_migration_policy: None,
        },
    );

    let bin_map = binary_map(&cfg.bins)?;
    for spec in RELEASE_BINARIES {
        let bin_path = bin_map
            .get(spec.binary_name)
            .ok_or_else(|| anyhow!("missing required binary {}", spec.binary_name))?;
        let relative = format!("bin/{}", spec.binary_name);
        let file = copy_file_with_hash(
            bin_path,
            &package_dir.join(&relative),
            &relative,
            spec.file_unit,
            &mut output_paths,
        )?;
        let db_migration_policy = if spec.unit == UnitName::CalmServer {
            Some(db_migration_policy()?)
        } else {
            None
        };
        units.insert(
            spec.unit,
            ReleaseUnit {
                version: version_from_binary(bin_path)?,
                binary_sha256: Some(file.sha256.clone()),
                tree_sha256: None,
                restart_policy: spec.restart_policy,
                db_migration_policy,
            },
        );
        files.push(file);
    }

    let calm_server = bin_map
        .get("calm-server")
        .ok_or_else(|| anyhow!("missing required binary calm-server"))?;
    let manifest = ReleaseManifestV2 {
        schema_version: 2,
        release_id: cfg.release_id.clone(),
        product_major: product_major()?,
        compatibility: compatibility_from_kernel(calm_server)?,
        units,
        files,
    };
    let mut manifest = manifest;
    manifest.files.sort_by(|a, b| a.path.cmp(&b.path));
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    fs::write(package_dir.join("manifest.json"), manifest_bytes)
        .with_context(|| format!("write {}", package_dir.join("manifest.json").display()))?;

    Ok(package_dir)
}

pub(crate) fn parse_named_path(value: &str) -> Result<NamedPath, String> {
    let Some((name, path)) = value.split_once('=') else {
        return Err("expected NAME=PATH".into());
    };
    validate_component_name("binary name", name)?;
    if path.is_empty() {
        return Err("binary path must not be empty".into());
    }
    Ok(NamedPath {
        name: name.to_string(),
        path: PathBuf::from(path),
    })
}

pub(crate) fn validate_release_id(release_id: &str) -> Result<(), String> {
    validate_component_name("release_id", release_id)
}

fn validate_component_name(kind: &str, name: &str) -> Result<(), String> {
    if name.is_empty() {
        return Err(format!("{kind} must not be empty"));
    }
    if name == "." || name == ".." {
        return Err(format!("{kind} must not be . or .."));
    }
    if !name
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(format!("{kind} must match [A-Za-z0-9._-]+"));
    }
    Ok(())
}

fn resolve_package_dir(release_dir: &Path, out: Option<&Path>) -> anyhow::Result<PathBuf> {
    if let Some(out) = out {
        let name = release_dir
            .file_name()
            .ok_or_else(|| anyhow!("--release-dir must have a final path component"))?;
        Ok(out.join(name))
    } else {
        Ok(release_dir.to_path_buf())
    }
}

fn ensure_fresh_dir(path: &Path) -> anyhow::Result<()> {
    if path.exists() {
        let mut entries = fs::read_dir(path)
            .with_context(|| format!("inspect existing package dir {}", path.display()))?;
        if entries.next().is_some() {
            return Err(anyhow!(
                "package dir {} already exists and is not empty",
                path.display()
            ));
        }
    }
    fs::create_dir_all(path).with_context(|| format!("create package dir {}", path.display()))?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
struct ReleaseBinarySpec {
    binary_name: &'static str,
    unit: UnitName,
    restart_policy: RestartPolicy,
    file_unit: FileUnit,
}

const RELEASE_BINARIES: &[ReleaseBinarySpec] = &[
    ReleaseBinarySpec {
        binary_name: "calm-server",
        unit: UnitName::CalmServer,
        restart_policy: RestartPolicy::RestartViaAdminApi,
        file_unit: FileUnit::CalmServer,
    },
    ReleaseBinarySpec {
        binary_name: "calm-proc-supervisor",
        unit: UnitName::CalmProcSupervisor,
        restart_policy: RestartPolicy::DeferUntilFullReboot,
        file_unit: FileUnit::Bundle,
    },
    ReleaseBinarySpec {
        binary_name: "neige-codex-bridge",
        unit: UnitName::NeigeCodexBridge,
        restart_policy: RestartPolicy::NextSpawn,
        file_unit: FileUnit::Bundle,
    },
    ReleaseBinarySpec {
        binary_name: "neige-mcp-stdio-shim",
        unit: UnitName::NeigeMcpStdioShim,
        restart_policy: RestartPolicy::NextSpawn,
        file_unit: FileUnit::Bundle,
    },
    ReleaseBinarySpec {
        binary_name: "neige",
        unit: UnitName::NeigeCli,
        restart_policy: RestartPolicy::NextSpawn,
        file_unit: FileUnit::Bundle,
    },
];

fn binary_map(bins: &[NamedPath]) -> anyhow::Result<BTreeMap<&str, &Path>> {
    let mut map = BTreeMap::new();
    for bin in bins {
        validate_component_name("binary name", &bin.name).map_err(|err| anyhow!("{err}"))?;
        if map.insert(bin.name.as_str(), bin.path.as_path()).is_some() {
            return Err(anyhow!("duplicate binary name {}", bin.name));
        }
    }
    Ok(map)
}

fn version_from_binary(bin: &Path) -> anyhow::Result<String> {
    let output = StdCommand::new(bin)
        .arg("--version")
        .output()
        .with_context(|| format!("run {} --version", bin.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "{} --version failed: {}",
            bin.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8(output.stdout)
        .with_context(|| format!("{} --version output was not UTF-8", bin.display()))?;
    parse_version_output(&stdout).ok_or_else(|| {
        anyhow!(
            "could not parse semver from {} --version output {:?}",
            bin.display(),
            stdout.trim()
        )
    })
}

fn compatibility_from_kernel(calm_server: &Path) -> anyhow::Result<Compatibility> {
    let output = StdCommand::new(calm_server)
        .arg("--emit-version-json")
        .output()
        .with_context(|| format!("run {} --emit-version-json", calm_server.display()))?;
    if !output.status.success() {
        return Err(anyhow!(
            "{} --emit-version-json failed: {}",
            calm_server.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "parse compatibility fields from {} --emit-version-json",
            calm_server.display()
        )
    })
}

fn product_major() -> anyhow::Result<u32> {
    match std::env::var("NEIGE_PRODUCT_MAJOR") {
        Ok(value) => value
            .parse()
            .with_context(|| format!("parse NEIGE_PRODUCT_MAJOR={value:?} as u32")),
        Err(std::env::VarError::NotPresent) => Ok(0),
        Err(err) => Err(err).context("read NEIGE_PRODUCT_MAJOR"),
    }
}

fn db_migration_policy() -> anyhow::Result<DbMigrationPolicy> {
    match std::env::var("NEIGE_DB_MIGRATION_POLICY") {
        Ok(value) => parse_db_migration_policy(&value)
            .map_err(|err| anyhow!("invalid NEIGE_DB_MIGRATION_POLICY: {err}")),
        Err(std::env::VarError::NotPresent) => Ok(DbMigrationPolicy::ForwardOnly),
        Err(err) => Err(err).context("read NEIGE_DB_MIGRATION_POLICY"),
    }
}

fn parse_db_migration_policy(value: &str) -> Result<DbMigrationPolicy, String> {
    match value {
        "none" => Ok(DbMigrationPolicy::None),
        "additive" => Ok(DbMigrationPolicy::Additive),
        "forwardOnly" => Ok(DbMigrationPolicy::ForwardOnly),
        "destructive" => Ok(DbMigrationPolicy::Destructive),
        other => Err(format!(
            "expected one of none, additive, forwardOnly, destructive; got {other:?}"
        )),
    }
}

#[derive(Deserialize)]
struct WebPackageJson {
    version: String,
}

fn web_package_version(web_dist: &Path) -> anyhow::Result<String> {
    let web_dir = web_dist
        .parent()
        .ok_or_else(|| anyhow!("web_dist must have a parent directory"))?;
    let path = web_dir.join("package.json");
    let package: WebPackageJson = serde_json::from_slice(
        &fs::read(&path).with_context(|| format!("read {}", path.display()))?,
    )
    .with_context(|| format!("parse {}", path.display()))?;
    Ok(package.version)
}

fn tree_sha256(root: &Path) -> anyhow::Result<String> {
    if !root.is_dir() {
        return Err(anyhow!("{} is not a directory", root.display()));
    }
    let mut files = Vec::new();
    collect_tree_files(root, root, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = Sha256::new();
    for (relative, path) in files {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        let mut file = fs::File::open(&path).with_context(|| format!("open {}", path.display()))?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .with_context(|| format!("read {}", path.display()))?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        hasher.update([0]);
    }
    Ok(hex_lower(&hasher.finalize()))
}

fn collect_tree_files(
    root: &Path,
    dir: &Path,
    files: &mut Vec<(String, PathBuf)>,
) -> anyhow::Result<()> {
    let mut entries = fs::read_dir(dir)
        .with_context(|| format!("read dir {}", dir.display()))?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let path = entry.path();
        if path.is_dir() {
            collect_tree_files(root, &path, files)?;
        } else if path.is_file() {
            let relative = path
                .strip_prefix(root)
                .with_context(|| format!("strip {} prefix", root.display()))?
                .to_string_lossy()
                .replace('\\', "/");
            files.push((relative, path));
        }
    }
    Ok(())
}

fn copy_dir_with_hashes(
    src: &Path,
    dst: &Path,
    relative_prefix: &str,
    unit: FileUnit,
    files: &mut Vec<FileManifest>,
    output_paths: &mut HashSet<String>,
) -> anyhow::Result<()> {
    if !src.is_dir() {
        return Err(anyhow!("{} is not a directory", src.display()));
    }
    let mut entries = fs::read_dir(src)
        .with_context(|| format!("read dir {}", src.display()))?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let src_path = entry.path();
        let file_name = entry.file_name();
        let dst_path = dst.join(&file_name);
        let relative_path = format!("{relative_prefix}/{}", file_name.to_string_lossy());
        if src_path.is_dir() {
            copy_dir_with_hashes(
                &src_path,
                &dst_path,
                &relative_path,
                unit,
                files,
                output_paths,
            )?;
        } else if src_path.is_file() {
            let file =
                copy_file_with_hash(&src_path, &dst_path, &relative_path, unit, output_paths)?;
            files.push(file);
        }
    }
    Ok(())
}

fn copy_file_with_hash(
    src: &Path,
    dst: &Path,
    relative_path: &str,
    unit: FileUnit,
    output_paths: &mut HashSet<String>,
) -> anyhow::Result<FileManifest> {
    if !src.is_file() {
        return Err(anyhow!("{} is not a file", src.display()));
    }
    let relative_path = relative_path.replace('\\', "/");
    if !output_paths.insert(relative_path.clone()) {
        return Err(anyhow!("duplicate package output path {relative_path}"));
    }
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create dir {}", parent.display()))?;
    }
    fs::copy(src, dst).with_context(|| format!("copy {} to {}", src.display(), dst.display()))?;
    let (sha256, bytes) = sha256_file(dst)?;
    Ok(FileManifest {
        path: relative_path,
        sha256,
        bytes,
        unit,
    })
}

pub(crate) fn sha256_file(path: &Path) -> anyhow::Result<(String, u64)> {
    let mut file = fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let bytes = fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    Ok((hex_lower(&hasher.finalize()), bytes))
}

fn hex_lower(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{ReleaseManifestV2, RestartPolicy, UnitName};
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn package_directory_contains_v2_manifest_and_hashes() {
        let tmp = test_temp_dir("package-smoke");
        let src = fake_build_output(&tmp);

        let package_dir = build_package(&PackageConfig {
            release_dir: tmp.join("pkg"),
            out: None,
            release_id: "smoke".into(),
            app_version: None,
            app_bin: Some(src.join("neige-app")),
            web_dist: Some(src.join("web").join("dist")),
            web_version: None,
            calm_server_version: None,
            db_migration_policy: DbMigrationPolicy::ForwardOnly,
            compatibility: compat_v1(),
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
        assert_eq!(manifest.product_major, 0);
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
            app_version: None,
            app_bin: Some(src.join("neige-app")),
            web_dist: Some(src.join("web").join("dist")),
            web_version: None,
            calm_server_version: None,
            db_migration_policy: DbMigrationPolicy::None,
            compatibility: compat_v1(),
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
            app_version: None,
            app_bin: Some(src.join("neige-app")),
            web_dist: Some(src.join("web").join("dist")),
            web_version: None,
            calm_server_version: None,
            db_migration_policy: DbMigrationPolicy::None,
            compatibility: compat_v1(),
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
            app_version: None,
            app_bin: Some(src.join("neige-app")),
            web_dist: Some(src.join("web").join("dist")),
            web_version: None,
            calm_server_version: None,
            db_migration_policy: DbMigrationPolicy::None,
            compatibility: compat_v1(),
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
  --emit-version-json) cat <<'JSON'
{"kernelVersion":"0.1.0","terminalFrameVersion":4,"terminalProtocolVersion":4,"apiVersion":"1","syncEventVersion":1,"mcpProtocolVersion":"2024-11-05","pluginMcpProtocolVersion":"2025-11-25","webCompatVersion":2,"minWebCompatVersion":2,"supervisorControlVersion":1,"buildSha":null,"dbInstanceId":"test"}
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

    fn compat_v1() -> CompatibilityV1 {
        CompatibilityV1 {
            api_version: "1".into(),
            sync_event_version: 1,
            mcp_protocol_version: "2024-11-05".into(),
            web_compat_version: 2,
            min_web_compat_version: 2,
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
}
