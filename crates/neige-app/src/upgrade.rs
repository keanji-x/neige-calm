use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command as StdCommand;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, anyhow};
use serde::{Deserialize, Serialize};

use crate::config::AppConfig;
use crate::installed::{InstalledState, read_installed_state, write_installed_state};
use crate::manifest::{CurrentVersion, VersionedReleaseManifest, parse_versioned_manifest};
use crate::package::{sha256_file, validate_release_id};
use crate::preflight::{self, PreflightMode, PreflightResult, Verdict};

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpgradeStageResult {
    pub staged: bool,
    pub mode: String,
    pub release_id: String,
    pub stage_dir: PathBuf,
    pub preflight: PreflightResult,
    pub restart_required: bool,
    pub required_action: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ActivationResult {
    pub activated: bool,
    pub mode: String,
    pub release_id: String,
    pub restart_required: bool,
    pub changed_symlinks: Vec<SymlinkActivationChange>,
    pub db_backup: Option<PathBuf>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RollbackResult {
    pub rolled_back: bool,
    pub mode: String,
    pub restart_required: bool,
    pub changed_symlinks: Vec<SymlinkRollbackChange>,
    pub warning: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SymlinkActivationChange {
    pub role: String,
    pub current: PathBuf,
    pub previous: PathBuf,
    pub old_current: Option<PathBuf>,
    pub old_previous: Option<PathBuf>,
    pub new_current: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SymlinkRollbackChange {
    pub role: String,
    pub current: PathBuf,
    pub previous: PathBuf,
    pub restored_current: Option<PathBuf>,
    pub restored_previous: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ActivationMetadata {
    version: u32,
    mode: String,
    release_id: String,
    restart_required: bool,
    db_backup: Option<PathBuf>,
    changed_symlinks: Vec<SymlinkActivationChange>,
}

#[derive(Debug, Clone)]
struct SymlinkPair {
    role: &'static str,
    current: PathBuf,
    previous: PathBuf,
}

pub(crate) fn infer_package_mode(package_dir: &Path) -> anyhow::Result<PreflightMode> {
    match read_versioned_manifest(package_dir)? {
        VersionedReleaseManifest::V1(manifest) => {
            preflight::infer_mode(&manifest).map_err(|err| anyhow!("{err}"))
        }
        VersionedReleaseManifest::V2(manifest) => mode_for_v2_manifest(&manifest),
    }
}

pub(crate) fn read_versioned_manifest(
    package_dir: &Path,
) -> anyhow::Result<VersionedReleaseManifest> {
    let manifest_path = package_dir.join("manifest.json");
    let bytes = fs::read(&manifest_path)
        .with_context(|| format!("read package manifest {}", manifest_path.display()))?;
    parse_versioned_manifest(&bytes)
        .with_context(|| format!("parse package manifest {}", manifest_path.display()))
}

pub(crate) fn stage_upgrade(
    cfg: &AppConfig,
    package_dir: &Path,
    mode: PreflightMode,
) -> anyhow::Result<UpgradeStageResult> {
    let manifest = read_versioned_manifest(package_dir)?;
    validate_release_id(manifest.release_id()).map_err(|err| anyhow!("{err}"))?;
    let verified_files = verify_package_hashes(package_dir, &manifest)?;
    reject_unmanifested_payload(package_dir, &verified_files)?;

    let preflight = match &manifest {
        VersionedReleaseManifest::V1(manifest) => {
            let current = current_version_for_mode(cfg, mode)?;
            preflight::run_preflight(mode, &current, manifest)
        }
        VersionedReleaseManifest::V2(manifest) => {
            let installed = read_installed_state(&cfg.calm_data_dir_resolved())?;
            let verdict = preflight::run_preflight_v2(installed.as_ref(), manifest);
            PreflightResult::from_verdict(mode_for_verdict(&verdict, mode), verdict)
        }
    };
    if !preflight.allowed && preflight.verdict.is_none() {
        return Err(anyhow!(
            "preflight denied staged upgrade: {} ({})",
            preflight.reason,
            preflight.required_action
        ));
    }

    let stage_root = cfg.release.root.join("staged");
    let release_id = manifest.release_id().to_string();
    let stage_dir = stage_root.join(&release_id);
    if !stage_dir.starts_with(&stage_root) {
        return Err(anyhow!(
            "stage dir {} escapes stage root {}",
            stage_dir.display(),
            stage_root.display()
        ));
    }
    ensure_empty_target(&stage_dir)?;
    copy_verified_files(package_dir, &stage_dir, &verified_files)?;

    Ok(UpgradeStageResult {
        staged: true,
        mode: preflight.mode.clone(),
        release_id,
        stage_dir,
        restart_required: restart_required_for_preflight(&preflight)?,
        required_action: stage_required_action_for_preflight(&preflight)?,
        preflight,
    })
}

pub(crate) fn activate_staged_release(
    cfg: &AppConfig,
    stage_dir: &Path,
    preflight: &PreflightResult,
    release_id: &str,
) -> anyhow::Result<ActivationResult> {
    validate_staged_release_target(cfg, stage_dir)?;
    let mode = mode_from_preflight(preflight)?;
    let pairs = symlink_pairs_for_mode(cfg, mode)?;
    prevalidate_symlink_pairs(&pairs)?;
    let db_backup = if preflight.requires_db_backup {
        Some(backup_sqlite_db(cfg, release_id)?)
    } else {
        None
    };

    let mut changed_symlinks = Vec::new();
    for pair in &pairs {
        let old_current = read_symlink_if_exists(&pair.current)?
            .map(|target| resolve_link_target(&pair.current, &target));
        let old_previous = read_symlink_if_exists(&pair.previous)?
            .map(|target| resolve_link_target(&pair.previous, &target));
        changed_symlinks.push(SymlinkActivationChange {
            role: pair.role.into(),
            current: pair.current.clone(),
            previous: pair.previous.clone(),
            old_current,
            old_previous,
            new_current: stage_dir.to_path_buf(),
        });
    }

    remove_stale_activation_metadata(cfg)?;
    for (pair, change) in pairs.iter().zip(&changed_symlinks) {
        if let Some(old_target) = &change.old_current {
            replace_symlink_atomic(&pair.previous, old_target)?;
        } else {
            remove_symlink_if_exists(&pair.previous)?;
        }
        replace_symlink_atomic(&pair.current, stage_dir)?;
    }
    let restart_required = restart_required_for_mode(mode);
    write_activation_metadata(
        cfg,
        &ActivationMetadata {
            version: 1,
            mode: mode.as_str().into(),
            release_id: release_id.into(),
            restart_required,
            db_backup: db_backup.clone(),
            changed_symlinks: changed_symlinks.clone(),
        },
    )?;
    if let VersionedReleaseManifest::V2(manifest) = read_versioned_manifest(stage_dir)? {
        let installed = InstalledState::from_manifest(&manifest);
        write_installed_state(&cfg.calm_data_dir_resolved(), &installed)?;
    }
    Ok(ActivationResult {
        activated: true,
        mode: mode.as_str().into(),
        release_id: release_id.into(),
        restart_required,
        changed_symlinks,
        db_backup,
    })
}

pub(crate) fn rollback_current(cfg: &AppConfig) -> anyhow::Result<RollbackResult> {
    if activation_metadata_path(cfg).exists() {
        return rollback_last_activation(cfg);
    }
    let previous_target = std::fs::read_link(&cfg.release.previous_server).with_context(|| {
        format!(
            "read previous server symlink {}",
            cfg.release.previous_server.display()
        )
    })?;
    let previous_target = resolve_link_target(&cfg.release.previous_server, &previous_target);
    validate_staged_release_target(cfg, &previous_target)?;
    replace_symlink_atomic(&cfg.release.current_server, &previous_target)?;
    Ok(RollbackResult {
        rolled_back: true,
        mode: "server-only".into(),
        restart_required: true,
        changed_symlinks: vec![SymlinkRollbackChange {
            role: "server".into(),
            current: cfg.release.current_server.clone(),
            previous: cfg.release.previous_server.clone(),
            restored_current: Some(previous_target),
            restored_previous: read_symlink_if_exists(&cfg.release.previous_server)?
                .map(|target| resolve_link_target(&cfg.release.previous_server, &target)),
        }],
        warning: "DB restore is not implemented; rollback only switched release symlinks".into(),
    })
}

fn rollback_last_activation(cfg: &AppConfig) -> anyhow::Result<RollbackResult> {
    let metadata_path = activation_metadata_path(cfg);
    let metadata: ActivationMetadata = read_json(&metadata_path)
        .with_context(|| format!("read activation metadata {}", metadata_path.display()))?;
    if metadata.version != 1 {
        return Err(anyhow!(
            "unsupported activation metadata version {}",
            metadata.version
        ));
    }
    let mode = activation_mode_from_str(&metadata.mode)?;
    let expected_pairs = symlink_pairs_for_mode(cfg, mode)?;
    validate_activation_metadata(cfg, &metadata, &expected_pairs)?;
    prevalidate_symlink_pairs(&expected_pairs)?;

    let mut changed_symlinks = Vec::new();
    for (pair, change) in expected_pairs.iter().zip(&metadata.changed_symlinks).rev() {
        if let Some(old_current) = &change.old_current {
            replace_symlink_atomic(&pair.current, old_current)?;
        } else {
            remove_symlink_if_exists(&pair.current)?;
        }
        if let Some(old_previous) = &change.old_previous {
            replace_symlink_atomic(&pair.previous, old_previous)?;
        } else {
            remove_symlink_if_exists(&pair.previous)?;
        }
        changed_symlinks.push(SymlinkRollbackChange {
            role: change.role.clone(),
            current: pair.current.clone(),
            previous: pair.previous.clone(),
            restored_current: change.old_current.clone(),
            restored_previous: change.old_previous.clone(),
        });
    }
    fs::remove_file(&metadata_path)
        .with_context(|| format!("remove activation metadata {}", metadata_path.display()))?;

    Ok(RollbackResult {
        rolled_back: true,
        mode: mode.as_str().into(),
        restart_required: restart_required_for_mode(mode),
        changed_symlinks,
        warning: "DB restore is not implemented; rollback only switched release symlinks".into(),
    })
}

fn current_version_for_mode(
    cfg: &AppConfig,
    mode: PreflightMode,
) -> anyhow::Result<CurrentVersion> {
    if matches!(mode, PreflightMode::Bundle | PreflightMode::AppOnly) {
        return Ok(CurrentVersion {
            api_version: "unused-for-mode".into(),
            sync_event_version: 0,
            mcp_protocol_version: "unused-for-mode".into(),
            min_web_compat_version: 0,
            web_compat_version: Some(0),
            plugin_mcp_protocol_version: None,
            supervisor_control_version: None,
        });
    }
    let path = cfg.upgrade.current_version_file.as_ref().ok_or_else(|| {
        anyhow!(
            "upgrade.current_version_file is required for {} preflight",
            mode.as_str()
        )
    })?;
    read_json(path).with_context(|| format!("read current version {}", path.display()))
}

fn mode_from_preflight(preflight: &PreflightResult) -> anyhow::Result<PreflightMode> {
    activation_mode_from_str(&preflight.mode)
}

fn restart_required_for_preflight(preflight: &PreflightResult) -> anyhow::Result<bool> {
    if let Some(verdict) = &preflight.verdict {
        return Ok(restart_required_for_verdict(verdict));
    }
    Ok(restart_required_for_mode(staging_mode_from_str(
        &preflight.mode,
    )?))
}

fn stage_required_action_for_preflight(preflight: &PreflightResult) -> anyhow::Result<String> {
    if let Some(verdict) = &preflight.verdict {
        return Ok(stage_required_action_for_verdict(verdict).into());
    }
    Ok(stage_required_action(staging_mode_from_str(&preflight.mode)?).into())
}

fn mode_for_v2_manifest(
    manifest: &crate::manifest::ReleaseManifestV2,
) -> anyhow::Result<PreflightMode> {
    let has_web = manifest.units.contains_key(&crate::manifest::UnitName::Web);
    let has_backend = manifest.units.keys().any(|unit| {
        !matches!(
            unit,
            crate::manifest::UnitName::Web | crate::manifest::UnitName::NeigeApp
        )
    });
    let has_app_only = manifest.units.len() == 1
        && manifest
            .units
            .contains_key(&crate::manifest::UnitName::NeigeApp);

    match (has_app_only, has_web, has_backend) {
        (true, false, false) => Ok(PreflightMode::AppOnly),
        (false, true, false) => Ok(PreflightMode::WebOnly),
        (false, false, true) => Ok(PreflightMode::ServerOnly),
        (false, true, true) => Ok(PreflightMode::Bundle),
        _ => Err(anyhow!(
            "unable to infer upgrade mode from manifest v2 units"
        )),
    }
}

fn mode_for_verdict(verdict: &Verdict, fallback: PreflightMode) -> PreflightMode {
    let units = match verdict {
        Verdict::Noop => return fallback,
        Verdict::Preserving { units_changed, .. } | Verdict::Breaking { units_changed, .. } => {
            units_changed
        }
    };
    let has_web = units.contains(&crate::manifest::UnitName::Web);
    let has_backend = units.iter().any(|unit| {
        !matches!(
            unit,
            crate::manifest::UnitName::Web | crate::manifest::UnitName::NeigeApp
        )
    });
    match (has_web, has_backend) {
        (true, true) => PreflightMode::Bundle,
        (true, false) => PreflightMode::WebOnly,
        (false, true) => PreflightMode::ServerOnly,
        (false, false) => fallback,
    }
}

fn activation_mode_from_str(mode: &str) -> anyhow::Result<PreflightMode> {
    match mode {
        "web-only" => Ok(PreflightMode::WebOnly),
        "server-only" => Ok(PreflightMode::ServerOnly),
        "bundle" => Ok(PreflightMode::Bundle),
        "app-only" => Err(anyhow!("app-only self-upgrade activation is not supported")),
        other => Err(anyhow!("unsupported preflight mode {other}")),
    }
}

fn staging_mode_from_str(mode: &str) -> anyhow::Result<PreflightMode> {
    match mode {
        "web-only" => Ok(PreflightMode::WebOnly),
        "server-only" => Ok(PreflightMode::ServerOnly),
        "bundle" => Ok(PreflightMode::Bundle),
        "app-only" => Ok(PreflightMode::AppOnly),
        other => Err(anyhow!("unsupported preflight mode {other}")),
    }
}

fn restart_required_for_verdict(verdict: &Verdict) -> bool {
    match verdict {
        Verdict::Noop => false,
        Verdict::Breaking { .. } => true,
        Verdict::Preserving {
            units_changed,
            deferred,
            refresh_frontend,
            ..
        } => {
            if units_changed.len() == deferred.len() {
                return false;
            }
            let has_restart_unit = units_changed
                .iter()
                .filter(|unit| !deferred.contains(unit))
                .any(|unit| !matches!(unit, crate::manifest::UnitName::Web));
            !*refresh_frontend || has_restart_unit
        }
    }
}

fn stage_required_action_for_verdict(verdict: &Verdict) -> &'static str {
    match verdict {
        Verdict::Noop => "noop",
        Verdict::Breaking { .. } => "allow-breaking-upgrade",
        Verdict::Preserving {
            units_changed,
            deferred,
            refresh_frontend,
            requires_db_backup,
        } => {
            if units_changed.len() == deferred.len() {
                "activate-staged-release-deferred-until-full-reboot"
            } else if *refresh_frontend && !restart_required_for_verdict(verdict) {
                "activate-staged-release-and-refresh-frontend"
            } else if *requires_db_backup {
                "backup-db-before-activate"
            } else if restart_required_for_verdict(verdict) {
                "activate-staged-release-and-restart-service"
            } else {
                "activate-staged-release"
            }
        }
    }
}

fn restart_required_for_mode(mode: PreflightMode) -> bool {
    matches!(mode, PreflightMode::ServerOnly | PreflightMode::Bundle)
}

fn stage_required_action(mode: PreflightMode) -> &'static str {
    match mode {
        PreflightMode::WebOnly => "activate-staged-release-and-refresh-frontend",
        PreflightMode::ServerOnly | PreflightMode::Bundle => {
            "activate-staged-release-and-restart-service"
        }
        PreflightMode::AppOnly => "activation-unsupported",
    }
}

fn symlink_pairs_for_mode(
    cfg: &AppConfig,
    mode: PreflightMode,
) -> anyhow::Result<Vec<SymlinkPair>> {
    if !matches!(mode, PreflightMode::AppOnly) {
        validate_split_release_paths(cfg)?;
    }
    match mode {
        PreflightMode::WebOnly => Ok(vec![SymlinkPair {
            role: "web",
            current: cfg.release.current_web.clone(),
            previous: cfg.release.previous_web.clone(),
        }]),
        PreflightMode::ServerOnly => Ok(vec![SymlinkPair {
            role: "server",
            current: cfg.release.current_server.clone(),
            previous: cfg.release.previous_server.clone(),
        }]),
        PreflightMode::Bundle => Ok(vec![
            SymlinkPair {
                role: "server",
                current: cfg.release.current_server.clone(),
                previous: cfg.release.previous_server.clone(),
            },
            SymlinkPair {
                role: "web",
                current: cfg.release.current_web.clone(),
                previous: cfg.release.previous_web.clone(),
            },
        ]),
        PreflightMode::AppOnly => Err(anyhow!("app-only self-upgrade activation is not supported")),
    }
}

fn validate_split_release_paths(cfg: &AppConfig) -> anyhow::Result<()> {
    if cfg.release.current_server == cfg.release.current_web {
        return Err(anyhow!(
            "release.current_server and release.current_web must be split paths for component activation"
        ));
    }
    if cfg.release.previous_server == cfg.release.previous_web {
        return Err(anyhow!(
            "release.previous_server and release.previous_web must be split paths for component activation"
        ));
    }
    Ok(())
}

fn prevalidate_symlink_pairs(pairs: &[SymlinkPair]) -> anyhow::Result<()> {
    let mut seen = HashSet::new();
    for pair in pairs {
        for path in [&pair.current, &pair.previous] {
            if !seen.insert(path.clone()) {
                return Err(anyhow!(
                    "release symlink path {} is used by more than one role",
                    path.display()
                ));
            }
            validate_symlink_slot(path)?;
        }
    }
    Ok(())
}

fn validate_symlink_slot(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(anyhow!("{} exists and is not a symlink", path.display())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("stat {}", path.display())),
    }
}

fn validate_activation_metadata(
    cfg: &AppConfig,
    metadata: &ActivationMetadata,
    expected_pairs: &[SymlinkPair],
) -> anyhow::Result<()> {
    if metadata.changed_symlinks.len() != expected_pairs.len() {
        return Err(anyhow!(
            "activation metadata for mode {} has {} symlink changes, expected {}",
            metadata.mode,
            metadata.changed_symlinks.len(),
            expected_pairs.len()
        ));
    }
    for (change, pair) in metadata.changed_symlinks.iter().zip(expected_pairs) {
        if change.role != pair.role {
            return Err(anyhow!(
                "activation metadata role {} does not match expected role {}",
                change.role,
                pair.role
            ));
        }
        if change.current != pair.current {
            return Err(anyhow!(
                "activation metadata current path {} does not match configured {}",
                change.current.display(),
                pair.current.display()
            ));
        }
        if change.previous != pair.previous {
            return Err(anyhow!(
                "activation metadata previous path {} does not match configured {}",
                change.previous.display(),
                pair.previous.display()
            ));
        }
        if let Some(old_current) = &change.old_current {
            validate_staged_release_target(cfg, old_current)?;
        }
        if let Some(old_previous) = &change.old_previous {
            validate_staged_release_target(cfg, old_previous)?;
        }
    }
    Ok(())
}

fn activation_metadata_path(cfg: &AppConfig) -> PathBuf {
    cfg.release.root.join("last-activation.json")
}

fn write_activation_metadata(cfg: &AppConfig, metadata: &ActivationMetadata) -> anyhow::Result<()> {
    let path = activation_metadata_path(cfg);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let bytes = serde_json::to_vec_pretty(metadata)?;
    let temp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    if temp.exists() {
        fs::remove_file(&temp).with_context(|| format!("remove {}", temp.display()))?;
    }
    fs::write(&temp, bytes).with_context(|| format!("write {}", temp.display()))?;
    fs::rename(&temp, &path)
        .with_context(|| format!("rename {} to {}", temp.display(), path.display()))
}

fn remove_stale_activation_metadata(cfg: &AppConfig) -> anyhow::Result<()> {
    let path = activation_metadata_path(cfg);
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

fn backup_sqlite_db(cfg: &AppConfig, release_id: &str) -> anyhow::Result<PathBuf> {
    let db_url = cfg.child.db_url.as_ref().ok_or_else(|| {
        anyhow!("DB backup required but child.db_url is not configured as sqlite://path?mode=rwc")
    })?;
    let db_path = parse_sqlite_file_url(db_url)?;
    if !db_path.is_file() {
        return Err(anyhow!("SQLite DB {} does not exist", db_path.display()));
    }
    std::fs::create_dir_all(&cfg.release.backups)
        .with_context(|| format!("create backup dir {}", cfg.release.backups.display()))?;
    let backup_path = cfg
        .release
        .backups
        .join(format!("{release_id}-{}-calm.db.bak", unix_ts()?));
    let backup_command = format!(".backup '{}'", sqlite_cli_quote(&backup_path));
    let status = StdCommand::new("sqlite3")
        .arg(&db_path)
        .arg(backup_command)
        .status()
        .with_context(|| "run sqlite3 online backup")?;
    if !status.success() {
        return Err(anyhow!("sqlite3 online backup failed with {status}"));
    }
    Ok(backup_path)
}

fn sqlite_cli_quote(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}

pub(crate) fn parse_sqlite_file_url(db_url: &str) -> anyhow::Result<PathBuf> {
    let Some(rest) = db_url.strip_prefix("sqlite://") else {
        return Err(anyhow!(
            "DB backup currently supports only sqlite://path?mode=rwc URLs"
        ));
    };
    let path = rest.split_once('?').map(|(path, _)| path).unwrap_or(rest);
    if path.is_empty() {
        return Err(anyhow!("sqlite DB path is empty"));
    }
    Ok(PathBuf::from(path))
}

fn verify_package_hashes(
    package_dir: &Path,
    manifest: &VersionedReleaseManifest,
) -> anyhow::Result<HashSet<String>> {
    if manifest.files().is_empty() {
        return Err(anyhow!("manifest files must not be empty"));
    }
    let mut verified = HashSet::new();
    for file in manifest.files() {
        validate_manifest_relative_path(&file.path)?;
        if !verified.insert(file.path.clone()) {
            return Err(anyhow!("duplicate manifest file path {}", file.path));
        }
        let path = package_dir.join(&file.path);
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("stat package file {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(anyhow!("manifest file {} is a symlink", file.path));
        }
        if !metadata.is_file() {
            return Err(anyhow!("manifest file {} is not a regular file", file.path));
        }
        let (actual_hash, actual_bytes) =
            sha256_file(&path).with_context(|| format!("hash package file {}", path.display()))?;
        if actual_hash != file.sha256.to_ascii_lowercase() {
            return Err(anyhow!("sha256 mismatch for {}", file.path));
        }
        if actual_bytes != file.bytes {
            return Err(anyhow!(
                "byte length mismatch for {}: expected {}, got {}",
                file.path,
                file.bytes,
                actual_bytes
            ));
        }
    }
    Ok(verified)
}

fn validate_manifest_relative_path(path: &str) -> anyhow::Result<()> {
    let path = Path::new(path);
    if path.is_absolute() {
        return Err(anyhow!(
            "manifest file path must be relative: {}",
            path.display()
        ));
    }
    for component in path.components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(anyhow!(
                "manifest file path contains unsafe component: {}",
                path.display()
            ));
        }
    }
    Ok(())
}

fn ensure_empty_target(path: &Path) -> anyhow::Result<()> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() {
            return Err(anyhow!("stage dir {} is a symlink", path.display()));
        }
        if !metadata.is_dir() {
            return Err(anyhow!(
                "stage dir {} exists and is not a directory",
                path.display()
            ));
        }
        let mut entries =
            fs::read_dir(path).with_context(|| format!("inspect stage dir {}", path.display()))?;
        if entries.next().is_some() {
            return Err(anyhow!(
                "stage dir {} already exists and is not empty",
                path.display()
            ));
        }
    } else if let Some(parent) = path.parent()
        && let Ok(parent_metadata) = fs::symlink_metadata(parent)
        && parent_metadata.file_type().is_symlink()
    {
        return Err(anyhow!("stage parent {} is a symlink", parent.display()));
    }
    Ok(())
}

fn reject_unmanifested_payload(
    package_dir: &Path,
    manifest_paths: &HashSet<String>,
) -> anyhow::Result<()> {
    reject_unmanifested_payload_inner(package_dir, package_dir, manifest_paths)
}

fn reject_unmanifested_payload_inner(
    root: &Path,
    dir: &Path,
    manifest_paths: &HashSet<String>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("stat package path {}", path.display()))?;
        if metadata.file_type().is_symlink() {
            return Err(anyhow!("package contains symlink {}", path.display()));
        }
        if metadata.is_dir() {
            reject_unmanifested_payload_inner(root, &path, manifest_paths)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("walked path must be under root")
                .to_string_lossy()
                .replace('\\', "/");
            if relative == "manifest.json" {
                continue;
            }
            if !manifest_paths.contains(&relative) {
                return Err(anyhow!("package contains unmanifested file {relative}"));
            }
        }
    }
    Ok(())
}

fn copy_verified_files(
    src: &Path,
    dst: &Path,
    manifest_paths: &HashSet<String>,
) -> anyhow::Result<()> {
    fs::create_dir_all(dst).with_context(|| format!("create {}", dst.display()))?;
    fs::copy(src.join("manifest.json"), dst.join("manifest.json")).with_context(|| {
        format!(
            "copy {} to {}",
            src.join("manifest.json").display(),
            dst.join("manifest.json").display()
        )
    })?;
    for relative in manifest_paths {
        let src_path = src.join(relative);
        let dst_path = dst.join(relative);
        if let Some(parent) = dst_path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        }
        fs::copy(&src_path, &dst_path)
            .with_context(|| format!("copy {} to {}", src_path.display(), dst_path.display()))?;
    }
    Ok(())
}

pub(crate) fn validate_staged_release_target(cfg: &AppConfig, target: &Path) -> anyhow::Result<()> {
    let metadata =
        fs::symlink_metadata(target).with_context(|| format!("stat {}", target.display()))?;
    if metadata.file_type().is_symlink() {
        return Err(anyhow!("release target {} is a symlink", target.display()));
    }
    if !metadata.is_dir() {
        return Err(anyhow!(
            "release target {} is not a directory",
            target.display()
        ));
    }

    let stage_root = cfg.release.root.join("staged");
    let canonical_stage_root = fs::canonicalize(&stage_root)
        .with_context(|| format!("canonicalize {}", stage_root.display()))?;
    let canonical_target =
        fs::canonicalize(target).with_context(|| format!("canonicalize {}", target.display()))?;
    if canonical_target == canonical_stage_root
        || !canonical_target.starts_with(&canonical_stage_root)
    {
        return Err(anyhow!(
            "release target {} is outside staged root {}",
            canonical_target.display(),
            canonical_stage_root.display()
        ));
    }

    let manifest = read_versioned_manifest(target)?;
    validate_release_id(manifest.release_id()).map_err(|err| anyhow!("{err}"))?;
    Ok(())
}

#[cfg(unix)]
pub(crate) fn replace_symlink_atomic(link: &Path, target: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::symlink;

    if let Some(parent) = link.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    if let Ok(metadata) = fs::symlink_metadata(link)
        && !metadata.file_type().is_symlink()
    {
        return Err(anyhow!("{} exists and is not a symlink", link.display()));
    }
    let temp = link.with_extension(format!("tmp.{}", std::process::id()));
    if temp.exists() {
        fs::remove_file(&temp).with_context(|| format!("remove {}", temp.display()))?;
    }
    symlink(target, &temp)
        .with_context(|| format!("create symlink {} -> {}", temp.display(), target.display()))?;
    fs::rename(&temp, link)
        .with_context(|| format!("rename {} to {}", temp.display(), link.display()))?;
    Ok(())
}

#[cfg(not(unix))]
pub(crate) fn replace_symlink_atomic(_link: &Path, _target: &Path) -> anyhow::Result<()> {
    Err(anyhow!("symlink activation is only implemented on Unix"))
}

pub(crate) fn read_symlink_if_exists(path: &Path) -> anyhow::Result<Option<PathBuf>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_symlink() {
                return Err(anyhow!("{} exists and is not a symlink", path.display()));
            }
            Ok(Some(fs::read_link(path)?))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("stat {}", path.display())),
    }
}

pub(crate) fn remove_symlink_if_exists(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_symlink() {
                return Err(anyhow!("{} exists and is not a symlink", path.display()));
            }
            fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
            Ok(())
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("stat {}", path.display())),
    }
}

pub(crate) fn resolve_link_target(link: &Path, target: &Path) -> PathBuf {
    if target.is_absolute() {
        target.to_path_buf()
    } else {
        link.parent().unwrap_or_else(|| Path::new(".")).join(target)
    }
}

fn unix_ts() -> anyhow::Result<u64> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
}

fn read_json<T>(path: &Path) -> anyhow::Result<T>
where
    T: for<'de> serde::Deserialize<'de>,
{
    let bytes = fs::read(path)?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AppConfig;
    use crate::installed::InstalledUnit;
    use crate::manifest::{
        Compatibility, CompatibilityV1, DbMigrationPolicy, FileManifest, FileUnit, ReleaseManifest,
        ReleaseManifestV2, ReleaseUnit, RestartPolicy, UnitName,
    };
    use crate::package::{NamedPath, PackageConfig, build_package};

    #[test]
    fn upgrade_stage_verifies_hash_and_copies_package() {
        let tmp = test_temp_dir("upgrade-stage");
        let src = tmp.join("src");
        fs::create_dir_all(src.join("web")).expect("create web");
        for name in [
            "calm-server",
            "neige-codex-bridge",
            "neige-mcp-stdio-shim",
            "neige",
        ] {
            fs::write(src.join(name), name).expect("write bin");
        }
        fs::write(src.join("web").join("index.html"), "web").expect("write web");
        let package_dir = build_package(&PackageConfig {
            release_dir: tmp.join("pkg"),
            out: None,
            release_id: "rel-1".into(),
            app_version: None,
            app_bin: None,
            web_dist: Some(src.join("web")),
            web_version: Some("web".into()),
            calm_server_version: Some("server".into()),
            db_migration_policy: DbMigrationPolicy::None,
            compatibility: compat(),
            bins: required_bins(&src),
        })
        .expect("package");

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
        let src = tmp.join("src");
        fs::create_dir_all(src.join("web")).expect("create web");
        fs::write(src.join("web").join("index.html"), "web").expect("write web");
        let package_dir = build_package(&PackageConfig {
            release_dir: tmp.join("pkg"),
            out: None,
            release_id: "rel-web".into(),
            app_version: None,
            app_bin: None,
            web_dist: Some(src.join("web")),
            web_version: Some("web".into()),
            calm_server_version: None,
            db_migration_policy: DbMigrationPolicy::None,
            compatibility: compat(),
            bins: Vec::new(),
        })
        .expect("package");

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
        let src = tmp.join("src");
        fs::create_dir_all(&src).expect("create src");
        for name in [
            "calm-server",
            "neige-codex-bridge",
            "neige-mcp-stdio-shim",
            "neige",
        ] {
            fs::write(src.join(name), name).expect("write bin");
        }
        let package_dir = build_package(&PackageConfig {
            release_dir: tmp.join("pkg"),
            out: None,
            release_id: "rel-server".into(),
            app_version: None,
            app_bin: None,
            web_dist: None,
            web_version: None,
            calm_server_version: Some("server".into()),
            db_migration_policy: DbMigrationPolicy::None,
            compatibility: compat(),
            bins: required_bins(&src),
        })
        .expect("package");

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
        let src = tmp.join("src");
        fs::create_dir_all(&src).expect("create src");
        let app_bin = src.join("neige-app");
        fs::write(&app_bin, "app").expect("write app");
        let package_dir = build_package(&PackageConfig {
            release_dir: tmp.join("pkg"),
            out: None,
            release_id: "rel-app".into(),
            app_version: Some("app".into()),
            app_bin: Some(app_bin),
            web_dist: None,
            web_version: None,
            calm_server_version: None,
            db_migration_policy: DbMigrationPolicy::None,
            compatibility: compat(),
            bins: Vec::new(),
        })
        .expect("package");

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
        std::os::unix::fs::symlink(&old_web, &cfg.release.current_web)
            .expect("web current symlink");
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
        std::os::unix::fs::symlink(&old_web, &cfg.release.current_web)
            .expect("web current symlink");
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
        std::os::unix::fs::symlink(&old_web, &cfg.release.current_web)
            .expect("web current symlink");
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
        std::os::unix::fs::symlink(&old_web, &cfg.release.current_web)
            .expect("web current symlink");
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
        std::os::unix::fs::symlink(&old_web, &cfg.release.current_web)
            .expect("web current symlink");
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
        std::os::unix::fs::symlink(&old_web, &cfg.release.current_web)
            .expect("web current symlink");
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
        std::os::unix::fs::symlink(&outside, &cfg.release.previous_server)
            .expect("previous symlink");

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
        let src = tmp.join("src");
        fs::create_dir_all(src.join("web")).expect("create web");
        for name in [
            "calm-server",
            "neige-codex-bridge",
            "neige-mcp-stdio-shim",
            "neige",
        ] {
            fs::write(src.join(name), name).expect("write bin");
        }
        fs::write(src.join("web").join("index.html"), "web").expect("write web");
        let package_dir = build_package(&PackageConfig {
            release_dir: tmp.join("pkg"),
            out: None,
            release_id: "rel-1".into(),
            app_version: None,
            app_bin: None,
            web_dist: Some(src.join("web")),
            web_version: Some("web".into()),
            calm_server_version: Some("server".into()),
            db_migration_policy: DbMigrationPolicy::None,
            compatibility: compat(),
            bins: required_bins(&src),
        })
        .expect("package");
        fs::write(package_dir.join("bin").join("neige"), "tampered").expect("tamper");

        let mut cfg = AppConfig::starter(tmp.join("config.toml"));
        cfg.release.root = tmp.join("releases");
        let err = stage_upgrade(&cfg, &package_dir, PreflightMode::Bundle)
            .expect_err("bad hash must fail");
        assert!(err.to_string().contains("sha256 mismatch"));
    }

    #[test]
    fn upgrade_stage_refuses_non_empty_target() {
        let tmp = test_temp_dir("upgrade-non-empty");
        let src = tmp.join("src");
        fs::create_dir_all(src.join("web")).expect("create web");
        for name in [
            "calm-server",
            "neige-codex-bridge",
            "neige-mcp-stdio-shim",
            "neige",
        ] {
            fs::write(src.join(name), name).expect("write bin");
        }
        fs::write(src.join("web").join("index.html"), "web").expect("write web");
        let package_dir = build_package(&PackageConfig {
            release_dir: tmp.join("pkg"),
            out: None,
            release_id: "rel-1".into(),
            app_version: None,
            app_bin: None,
            web_dist: Some(src.join("web")),
            web_version: Some("web".into()),
            calm_server_version: Some("server".into()),
            db_migration_policy: DbMigrationPolicy::None,
            compatibility: compat(),
            bins: required_bins(&src),
        })
        .expect("package");

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
        let src = tmp.join("src");
        fs::create_dir_all(src.join("web")).expect("create web");
        for name in [
            "calm-server",
            "neige-codex-bridge",
            "neige-mcp-stdio-shim",
            "neige",
        ] {
            fs::write(src.join(name), name).expect("write bin");
        }
        fs::write(src.join("web").join("index.html"), "web").expect("write web");
        let package_dir = build_package(&PackageConfig {
            release_dir: tmp.join("pkg"),
            out: None,
            release_id: "rel-1".into(),
            app_version: None,
            app_bin: None,
            web_dist: Some(src.join("web")),
            web_version: Some("web".into()),
            calm_server_version: Some("server".into()),
            db_migration_policy: DbMigrationPolicy::None,
            compatibility: compat(),
            bins: required_bins(&src),
        })
        .expect("package");

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
        let err = stage_upgrade(&cfg, &package_dir, PreflightMode::Bundle)
            .expect_err("extra file must fail");
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
        let err = stage_upgrade(&cfg, &package_dir, PreflightMode::Bundle)
            .expect_err("symlink must fail");
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

        let err = stage_upgrade(&cfg, &package_dir, PreflightMode::Bundle)
            .expect_err("stage file must fail");
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
        let (sha256, bytes) = sha256_file(&payload_path).expect("hash payload");
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

    fn required_bins(src: &Path) -> Vec<NamedPath> {
        [
            "calm-server",
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

    fn make_bundle_package(tmp: &Path) -> PathBuf {
        let src = tmp.join("src");
        fs::create_dir_all(src.join("web")).expect("create web");
        for name in [
            "calm-server",
            "neige-codex-bridge",
            "neige-mcp-stdio-shim",
            "neige",
        ] {
            fs::write(src.join(name), name).expect("write bin");
        }
        fs::write(src.join("web").join("index.html"), "web").expect("write web");
        build_package(&PackageConfig {
            release_dir: tmp.join("pkg"),
            out: None,
            release_id: "rel-1".into(),
            app_version: None,
            app_bin: None,
            web_dist: Some(src.join("web")),
            web_version: Some("web".into()),
            calm_server_version: Some("server".into()),
            db_migration_policy: DbMigrationPolicy::None,
            compatibility: compat(),
            bins: required_bins(&src),
        })
        .expect("package")
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
}
