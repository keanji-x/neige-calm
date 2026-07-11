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
use crate::package::{hash_and_measure_file, validate_release_id};
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
    let has_app_only = manifest.units.len() == 1
        && manifest
            .units
            .contains_key(&crate::manifest::UnitName::NeigeApp);
    let has_backend = !has_app_only
        && manifest
            .units
            .keys()
            .any(|unit| !matches!(unit, crate::manifest::UnitName::Web));

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
    let has_backend = units
        .iter()
        .any(|unit| !matches!(unit, crate::manifest::UnitName::Web));
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
        let (actual_hash, actual_bytes) = hash_and_measure_file(&path)
            .with_context(|| format!("hash package file {}", path.display()))?;
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
mod tests;
