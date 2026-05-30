use std::path::{Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};

use crate::config::{AppConfig, SourceConfig};
use crate::package::{NamedPath, PackageConfig};
use crate::preflight::PreflightMode;

const SOURCE_MARKER: &str = ".neige-app-source.json";

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceMarker {
    url: String,
    branch: String,
}

pub(crate) fn build_source_package(
    cfg: &AppConfig,
    mode_override: Option<PreflightMode>,
) -> anyhow::Result<PathBuf> {
    build_source_package_from_source(cfg, &cfg.source, mode_override)
}

pub(crate) fn build_source_package_from_source(
    cfg: &AppConfig,
    source: &SourceConfig,
    mode_override: Option<PreflightMode>,
) -> anyhow::Result<PathBuf> {
    source_mode_for(source, mode_override)?;
    let source_dir = prepare_source_checkout(source)?;
    run_build(&source_dir, &source.build_args)?;
    let release_id = format!("source-{}", unix_ts()?);
    let release_dir = cfg.release.root.join("packages").join(&release_id);
    crate::package::build_package(&PackageConfig {
        release_dir,
        out: None,
        release_id,
        app_bin: Some(source_dir.join("target").join("release").join("neige-app")),
        web_dist: Some(source_dir.join("web").join("dist")),
        bins: required_bins(&source_dir),
    })
}

pub(crate) fn source_mode(
    cfg: &AppConfig,
    mode_override: Option<PreflightMode>,
) -> anyhow::Result<Option<PreflightMode>> {
    source_mode_for(&cfg.source, mode_override)
}

pub(crate) fn source_mode_for(
    source: &SourceConfig,
    mode_override: Option<PreflightMode>,
) -> anyhow::Result<Option<PreflightMode>> {
    let mode = mode_override.or(source.mode);
    if matches!(mode, Some(PreflightMode::AppOnly)) {
        return Err(anyhow!(
            "source-driven app-only self-upgrade is not supported"
        ));
    }
    Ok(mode)
}

fn prepare_source_checkout(source: &SourceConfig) -> anyhow::Result<PathBuf> {
    let url = source
        .url
        .as_ref()
        .ok_or_else(|| anyhow!("source.url must be configured when --package is omitted"))?;
    let local_path = PathBuf::from(url);
    if local_path.exists() {
        return Ok(local_path);
    }

    let checkout = &source.checkout_dir;
    if checkout.exists() {
        verify_source_marker(checkout, url, &source.branch)?;
        verify_git_origin(checkout, url)?;
        run_git(
            checkout.parent().unwrap_or_else(|| Path::new(".")),
            &["-C", path_str(checkout)?, "fetch", "origin"],
        )?;
        run_git(
            checkout.parent().unwrap_or_else(|| Path::new(".")),
            &["-C", path_str(checkout)?, "checkout", &source.branch],
        )?;
        let target = format!("origin/{}", source.branch);
        run_git(
            checkout.parent().unwrap_or_else(|| Path::new(".")),
            &["-C", path_str(checkout)?, "reset", "--hard", &target],
        )?;
    } else {
        if let Some(parent) = checkout.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        run_git(
            checkout.parent().unwrap_or_else(|| Path::new(".")),
            &[
                "clone",
                "--branch",
                &source.branch,
                url,
                path_str(checkout)?,
            ],
        )?;
        write_source_marker(checkout, url, &source.branch)?;
    }
    Ok(checkout.clone())
}

fn verify_source_marker(checkout: &Path, url: &str, branch: &str) -> anyhow::Result<()> {
    let marker_path = checkout.join(SOURCE_MARKER);
    let marker: SourceMarker = serde_json::from_slice(
        &std::fs::read(&marker_path)
            .with_context(|| format!("read source marker {}", marker_path.display()))?,
    )
    .with_context(|| format!("parse source marker {}", marker_path.display()))?;
    if marker.url != url || marker.branch != branch {
        return Err(anyhow!(
            "checkout marker does not match config source url/branch"
        ));
    }
    Ok(())
}

fn verify_git_origin(checkout: &Path, url: &str) -> anyhow::Result<()> {
    let output = StdCommand::new("git")
        .args(["-C", path_str(checkout)?, "remote", "get-url", "origin"])
        .output()
        .with_context(|| "read git origin url")?;
    if !output.status.success() {
        return Err(anyhow!("git remote get-url origin failed"));
    }
    let origin = String::from_utf8(output.stdout)
        .context("git origin output was not UTF-8")?
        .trim()
        .to_string();
    if origin != url {
        return Err(anyhow!(
            "checkout origin {origin} does not match configured source url {url}"
        ));
    }
    Ok(())
}

fn write_source_marker(checkout: &Path, url: &str, branch: &str) -> anyhow::Result<()> {
    let marker = SourceMarker {
        url: url.into(),
        branch: branch.into(),
    };
    let path = checkout.join(SOURCE_MARKER);
    std::fs::write(&path, serde_json::to_vec_pretty(&marker)?)
        .with_context(|| format!("write source marker {}", path.display()))?;
    Ok(())
}

fn run_build(source_dir: &Path, build_args: &[String]) -> anyhow::Result<()> {
    let args: Vec<String> = if build_args.is_empty() {
        vec!["make".into(), "build".into()]
    } else {
        build_args.to_vec()
    };
    let (program, rest) = args
        .split_first()
        .ok_or_else(|| anyhow!("source.build_args must not be empty"))?;
    let status = StdCommand::new(program)
        .args(rest)
        .current_dir(source_dir)
        .status()
        .with_context(|| format!("run build command in {}", source_dir.display()))?;
    if !status.success() {
        return Err(anyhow!("build command failed with {status}"));
    }
    Ok(())
}

fn run_git(cwd: &Path, args: &[&str]) -> anyhow::Result<()> {
    let status = StdCommand::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .with_context(|| "run git")?;
    if !status.success() {
        return Err(anyhow!("git command failed with {status}"));
    }
    Ok(())
}

fn required_bins(source_dir: &Path) -> Vec<NamedPath> {
    [
        "calm-server",
        "neige-codex-bridge",
        "neige-mcp-stdio-shim",
        // Issue #388 Phase 1 — calm-server connects to this binary over a
        // control UDS for every terminal spawn; without it in the activated
        // release `neige-app system serve` will time out waiting for
        // `<current-server>/bin/calm-proc-supervisor` to come up.
        "calm-proc-supervisor",
        "neige",
    ]
    .into_iter()
    .map(|name| NamedPath {
        name: name.into(),
        path: source_dir.join("target").join("release").join(name),
    })
    .collect()
}

fn path_str(path: &Path) -> anyhow::Result<&str> {
    path.to_str()
        .ok_or_else(|| anyhow!("path is not valid UTF-8: {}", path.display()))
}

fn unix_ts() -> anyhow::Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn existing_checkout_without_marker_fails() {
        let tmp = test_temp_dir("source-marker");
        let mut cfg = AppConfig::starter(tmp.join("config.toml"));
        cfg.source.url = Some("https://example.com/repo.git".into());
        cfg.source.checkout_dir = tmp.join("checkout");
        std::fs::create_dir_all(&cfg.source.checkout_dir).expect("checkout dir");

        let err = prepare_source_checkout(&cfg.source).expect_err("missing marker must fail");
        assert!(err.to_string().contains("source marker"));
    }

    #[test]
    fn source_package_fails_when_built_artifacts_are_missing() {
        let tmp = test_temp_dir("source-config");
        let mut cfg = AppConfig::starter(tmp.join("config.toml"));
        cfg.source.url = Some(tmp.display().to_string());
        cfg.source.build_args = vec!["true".into()];

        let err = build_source_package(&cfg, None).expect_err("missing compat must fail");
        assert!(err.to_string().contains("neige-app"));
    }

    #[test]
    fn source_package_contains_v2_units() {
        let tmp = test_temp_dir("source-v2");
        let source = tmp.join("checkout");
        fake_build_output(&source);

        let mut cfg = AppConfig::starter(tmp.join("config.toml"));
        cfg.release.root = tmp.join("releases");
        cfg.source.url = Some(source.display().to_string());
        cfg.source.build_args = vec!["true".into()];

        let package = build_source_package(&cfg, None).expect("package");
        let manifest: crate::manifest::ReleaseManifestV2 = serde_json::from_slice(
            &std::fs::read(package.join("manifest.json")).expect("read manifest"),
        )
        .expect("parse manifest");

        assert_eq!(manifest.schema_version, 2);
        assert_eq!(manifest.units.len(), 7);
        assert!(
            manifest
                .units
                .contains_key(&crate::manifest::UnitName::CalmServer)
        );
        assert!(manifest.units.contains_key(&crate::manifest::UnitName::Web));
    }

    #[test]
    fn cli_mode_overrides_source_mode() {
        let tmp = test_temp_dir("source-mode-override");
        let mut cfg = AppConfig::starter(tmp.join("config.toml"));
        cfg.source.mode = Some(PreflightMode::Bundle);

        assert_eq!(
            source_mode(&cfg, Some(PreflightMode::WebOnly)).expect("mode"),
            Some(PreflightMode::WebOnly)
        );
    }

    fn test_temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("neige-app-{name}-{}", std::process::id()));
        if path.exists() {
            std::fs::remove_dir_all(&path).expect("remove stale temp dir");
        }
        std::fs::create_dir_all(&path).expect("create temp dir");
        path
    }

    fn fake_build_output(source: &Path) {
        let release = source.join("target").join("release");
        std::fs::create_dir_all(&release).expect("create release dir");
        std::fs::create_dir_all(source.join("web").join("dist")).expect("create web dist");
        std::fs::write(source.join("web").join("dist").join("index.html"), "web")
            .expect("write web");
        std::fs::write(
            source.join("web").join("package.json"),
            r#"{"version":"1.0.0"}"#,
        )
        .expect("write package json");
        write_script(
            &release.join("calm-server"),
            r#"case "$1" in
  --version) printf 'calm-server 1.0.0\n'; exit 0 ;;
  --emit-kernel-compatibility-json) cat <<'JSON'
{"terminalFrameVersion":4,"terminalProtocolVersion":4,"apiVersion":"1","syncEventVersion":1,"mcpProtocolVersion":"2024-11-05","pluginMcpProtocolVersion":"2025-11-25","webCompatVersion":2,"minWebCompatVersion":2,"supervisorControlVersion":1}
JSON
    exit 0 ;;
esac
exit 2
"#,
        );
        for name in [
            "neige-app",
            "calm-proc-supervisor",
            "neige-codex-bridge",
            "neige-mcp-stdio-shim",
            "neige",
        ] {
            write_script(
                &release.join(name),
                &format!(
                    r#"if [ "$1" = "--version" ]; then
  printf '{name} 1.0.0\n'
  exit 0
fi
exit 2
"#,
                ),
            );
        }
    }

    fn write_script(path: &Path, body: &str) {
        std::fs::write(path, format!("#!/bin/sh\n{body}")).expect("write script");
        let mut permissions = std::fs::metadata(path)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("chmod script");
    }
}
