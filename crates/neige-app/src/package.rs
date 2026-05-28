use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;

use anyhow::{Context, anyhow};

use crate::manifest::{
    AppUnit, BinaryUnit, BundleUnit, CalmServerUnit, Compatibility, DbMigrationPolicy,
    FileManifest, FileUnit, ReleaseManifest, ReleaseUnits, WebUnit,
};

#[derive(Debug, Clone)]
pub(crate) struct NamedPath {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
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
    pub compatibility: Compatibility,
    pub bins: Vec<NamedPath>,
}

pub(crate) fn build_package(cfg: &PackageConfig) -> anyhow::Result<PathBuf> {
    validate_release_id(&cfg.release_id).map_err(|err| anyhow!("{err}"))?;
    let package_dir = resolve_package_dir(&cfg.release_dir, cfg.out.as_deref())?;
    ensure_fresh_dir(&package_dir)?;

    let mut manifest = ReleaseManifest {
        schema_version: 1,
        release_id: cfg.release_id.clone(),
        units: ReleaseUnits::default(),
        files: Vec::new(),
    };
    let mut output_paths = HashSet::new();

    if let Some(app_bin) = &cfg.app_bin {
        copy_file_with_hash(
            app_bin,
            &package_dir.join("bin").join("neige-app"),
            "bin/neige-app",
            FileUnit::App,
            &mut manifest.files,
            &mut output_paths,
        )?;
        manifest.units.app = Some(AppUnit {
            name: "neige-app".into(),
            version: cfg.app_version.clone().unwrap_or_else(|| "unknown".into()),
        });
    } else if cfg.app_version.is_some() {
        manifest.units.app = Some(AppUnit {
            name: "neige-app".into(),
            version: cfg.app_version.clone().expect("checked above"),
        });
    }

    if let Some(web_dist) = &cfg.web_dist {
        copy_dir_with_hashes(
            web_dist,
            &package_dir.join("web").join("dist"),
            "web/dist",
            FileUnit::Web,
            &mut manifest.files,
            &mut output_paths,
        )?;
        manifest.units.web = Some(WebUnit {
            version: cfg.web_version.clone().unwrap_or_else(|| "unknown".into()),
            compatibility: cfg.compatibility.clone(),
        });
    }

    if !cfg.bins.is_empty() {
        let mut binaries = Vec::new();
        for bin in &cfg.bins {
            validate_component_name("binary name", &bin.name).map_err(|err| anyhow!("{err}"))?;
            let relative = format!("bin/{}", bin.name);
            copy_file_with_hash(
                &bin.path,
                &package_dir.join(&relative),
                &relative,
                if bin.name == "calm-server" {
                    FileUnit::CalmServer
                } else {
                    FileUnit::Bundle
                },
                &mut manifest.files,
                &mut output_paths,
            )?;
            binaries.push(BinaryUnit {
                name: bin.name.clone(),
                path: relative,
            });
        }
        manifest.units.bundle = Some(BundleUnit { binaries });

        if cfg.bins.iter().any(|bin| bin.name == "calm-server") {
            manifest.units.calm_server = Some(CalmServerUnit {
                version: cfg
                    .calm_server_version
                    .clone()
                    .unwrap_or_else(|| "unknown".into()),
                compatibility: cfg.compatibility.clone(),
                db_migration_policy: cfg.db_migration_policy,
            });
        }
    }

    if manifest.units.app.is_none()
        && manifest.units.web.is_none()
        && manifest.units.calm_server.is_none()
        && manifest.units.bundle.is_none()
    {
        return Err(anyhow!("package must include at least one unit"));
    }

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
    for entry in fs::read_dir(src).with_context(|| format!("read dir {}", src.display()))? {
        let entry = entry?;
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
            copy_file_with_hash(
                &src_path,
                &dst_path,
                &relative_path,
                unit,
                files,
                output_paths,
            )?;
        }
    }
    Ok(())
}

fn copy_file_with_hash(
    src: &Path,
    dst: &Path,
    relative_path: &str,
    unit: FileUnit,
    files: &mut Vec<FileManifest>,
    output_paths: &mut HashSet<String>,
) -> anyhow::Result<()> {
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
    files.push(FileManifest {
        path: relative_path,
        sha256,
        bytes,
        unit,
    });
    Ok(())
}

pub(crate) fn sha256_file(path: &Path) -> anyhow::Result<(String, u64)> {
    let output = StdCommand::new("sha256sum")
        .arg(path)
        .output()
        .with_context(|| "run sha256sum")?;
    if !output.status.success() {
        return Err(anyhow!(
            "sha256sum failed for {}: {}",
            path.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8(output.stdout).context("sha256sum output was not UTF-8")?;
    let hash = stdout
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow!("sha256sum produced no hash for {}", path.display()))?;
    if hash.len() != 64 || !hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(anyhow!(
            "sha256sum produced invalid hash for {}",
            path.display()
        ));
    }
    let bytes = fs::metadata(path)
        .with_context(|| format!("stat {}", path.display()))?
        .len();
    Ok((hash.to_ascii_lowercase(), bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_directory_contains_manifest_and_hashes() {
        let tmp = test_temp_dir("package-smoke");
        let src = tmp.join("src");
        fs::create_dir_all(src.join("web")).expect("create source web");
        fs::write(src.join("web").join("index.html"), "hello").expect("write web");
        fs::write(src.join("calm-server"), "server").expect("write server");
        fs::write(src.join("neige"), "cli").expect("write cli");

        let package_dir = build_package(&PackageConfig {
            release_dir: tmp.join("pkg"),
            out: None,
            release_id: "smoke".into(),
            app_version: None,
            app_bin: None,
            web_dist: Some(src.join("web")),
            web_version: Some("web-1".into()),
            calm_server_version: Some("server-1".into()),
            db_migration_policy: DbMigrationPolicy::ForwardOnly,
            compatibility: Compatibility {
                api_version: "1".into(),
                sync_event_version: 1,
                mcp_protocol_version: "2025-11-25".into(),
                web_compat_version: 2,
                min_web_compat_version: 2,
            },
            bins: vec![
                NamedPath {
                    name: "calm-server".into(),
                    path: src.join("calm-server"),
                },
                NamedPath {
                    name: "neige".into(),
                    path: src.join("neige"),
                },
            ],
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

        let manifest: ReleaseManifest = serde_json::from_slice(
            &fs::read(package_dir.join("manifest.json")).expect("read manifest"),
        )
        .expect("parse manifest");
        assert_eq!(manifest.release_id, "smoke");
        assert!(manifest.units.web.is_some());
        assert!(manifest.units.calm_server.is_some());
        assert!(manifest.units.bundle.is_some());
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
        let src = tmp.join("src");
        fs::create_dir_all(&src).expect("create source dir");
        fs::write(src.join("calm-server"), "server").expect("write server");

        let err = build_package(&PackageConfig {
            release_dir: tmp.join("pkg"),
            out: None,
            release_id: "../outside".into(),
            app_version: None,
            app_bin: None,
            web_dist: None,
            web_version: None,
            calm_server_version: Some("server-1".into()),
            db_migration_policy: DbMigrationPolicy::None,
            compatibility: Compatibility {
                api_version: "1".into(),
                sync_event_version: 1,
                mcp_protocol_version: "2025-11-25".into(),
                web_compat_version: 2,
                min_web_compat_version: 2,
            },
            bins: vec![NamedPath {
                name: "calm-server".into(),
                path: src.join("calm-server"),
            }],
        })
        .expect_err("unsafe release_id must fail");

        assert!(err.to_string().contains("release_id"));
    }

    #[test]
    fn package_rejects_duplicate_output_paths() {
        let tmp = test_temp_dir("duplicate-paths");
        let src = tmp.join("src");
        fs::create_dir_all(&src).expect("create source dir");
        fs::write(src.join("app"), "app").expect("write app");
        fs::write(src.join("other-app"), "other").expect("write other app");

        let err = build_package(&PackageConfig {
            release_dir: tmp.join("pkg"),
            out: None,
            release_id: "duplicate".into(),
            app_version: Some("app-1".into()),
            app_bin: Some(src.join("app")),
            web_dist: None,
            web_version: None,
            calm_server_version: None,
            db_migration_policy: DbMigrationPolicy::None,
            compatibility: Compatibility {
                api_version: "1".into(),
                sync_event_version: 1,
                mcp_protocol_version: "2025-11-25".into(),
                web_compat_version: 2,
                min_web_compat_version: 2,
            },
            bins: vec![NamedPath {
                name: "neige-app".into(),
                path: src.join("other-app"),
            }],
        })
        .expect_err("duplicate output path must be refused");

        assert!(err.to_string().contains("duplicate package output path"));
    }

    #[test]
    fn package_rejects_duplicate_bundle_binary_names() {
        let tmp = test_temp_dir("duplicate-bins");
        let src = tmp.join("src");
        fs::create_dir_all(&src).expect("create source dir");
        fs::write(src.join("one"), "one").expect("write one");
        fs::write(src.join("two"), "two").expect("write two");

        let err = build_package(&PackageConfig {
            release_dir: tmp.join("pkg"),
            out: None,
            release_id: "duplicate".into(),
            app_version: None,
            app_bin: None,
            web_dist: None,
            web_version: None,
            calm_server_version: Some("server-1".into()),
            db_migration_policy: DbMigrationPolicy::None,
            compatibility: Compatibility {
                api_version: "1".into(),
                sync_event_version: 1,
                mcp_protocol_version: "2025-11-25".into(),
                web_compat_version: 2,
                min_web_compat_version: 2,
            },
            bins: vec![
                NamedPath {
                    name: "calm-server".into(),
                    path: src.join("one"),
                },
                NamedPath {
                    name: "calm-server".into(),
                    path: src.join("two"),
                },
            ],
        })
        .expect_err("duplicate bin path must be refused");

        assert!(err.to_string().contains("duplicate package output path"));
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
