use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, anyhow};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::Supervisor;
use crate::config::{AppConfig, SourceConfig};
use crate::installed::{InstalledState, read_installed_state, write_installed_state};
use crate::manifest::{ReleaseManifestV2, RestartPolicy, UnitName, VersionedReleaseManifest};
use crate::preflight::{self, BreakingReason, Verdict};
use crate::source;
use crate::upgrade;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpgradeRequest {
    #[serde(default)]
    pub source: Option<SourceOverride>,
    #[serde(default)]
    pub allow_breaking: bool,
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(transparent)]
pub(crate) struct SourceOverride(serde_json::Map<String, serde_json::Value>);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct UpgradeResponse {
    pub release_id: String,
    pub verdict: VerdictSummary,
    pub result: UpgradeResult,
    pub units_changed: Vec<UnitName>,
    pub deferred: Vec<UnitName>,
    pub duration_ms: u64,
    pub error: Option<String>,
    pub release_history_entry: ReleaseHistoryEntry,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum UpgradeResult {
    Committed,
    RolledBack,
    Rejected,
    DryRun,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VerdictSummary {
    pub kind: String,
    pub units_changed: Vec<UnitName>,
    pub deferred: Vec<UnitName>,
    pub refresh_frontend: bool,
    pub requires_db_backup: bool,
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseHistoryEntry {
    #[serde(default = "default_history_kind")]
    pub kind: String,
    pub release_id: String,
    pub timestamp: String,
    pub verdict_kind: String,
    pub verdict_reason: Option<String>,
    pub units_changed: Vec<UnitName>,
    pub deferred: Vec<UnitName>,
    pub refresh_frontend: bool,
    pub requires_db_backup: bool,
    pub result: String,
    pub duration_ms: u64,
    pub error: Option<String>,
    pub source: SourceSummary,
    pub installed_at_before: Option<String>,
    pub installed_at_after: String,
    pub executed_breaking_self_exec: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symlink_changes: Vec<SymlinkPlanChange>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_backup: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SourceSummary {
    #[serde(rename = "type")]
    pub source_type: String,
    pub url: Option<String>,
    #[serde(rename = "ref")]
    pub ref_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SymlinkPlanChange {
    pub role: String,
    pub current: PathBuf,
    pub previous: PathBuf,
    pub old_current: Option<PathBuf>,
    pub old_previous: Option<PathBuf>,
    pub new_current: PathBuf,
}

#[derive(Debug)]
pub(crate) struct ApplyError {
    pub status: StatusCode,
    pub code: &'static str,
    pub message: String,
}

impl ApplyError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_upgrade_request",
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            code: "upgrade_apply_failed",
            message: message.into(),
        }
    }

    fn invalid_rollback_target(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid_rollback_target",
            message: message.into(),
        }
    }
}

impl From<anyhow::Error> for ApplyError {
    fn from(value: anyhow::Error) -> Self {
        Self::internal(value.to_string())
    }
}

pub(crate) async fn apply_upgrade(
    cfg: &AppConfig,
    supervisor: &Supervisor,
    proc_supervisor: &Supervisor,
    req: UpgradeRequest,
) -> Result<UpgradeResponse, ApplyError> {
    let started = Instant::now();
    let source = merge_source_override(&cfg.source, req.source.as_ref())?;
    let source_summary = SourceSummary {
        source_type: "git".into(),
        url: source.url.clone(),
        ref_name: source.branch.clone(),
    };
    let installed_before = read_installed_state_blocking(cfg).await?;
    let installed_before_id = installed_before
        .as_ref()
        .map(|state| state.release_id.clone());

    let package_dir = resolve_source_package_blocking(cfg, &source, !req.dry_run).await?;
    let source_manifest = read_v2_manifest_blocking(&package_dir).await?;
    let verdict = preflight::run_preflight_v2(installed_before.as_ref(), &source_manifest);
    let summary = VerdictSummary::from(&verdict);
    if req.dry_run {
        return Ok(response_from_parts(
            source_manifest.release_id.clone(),
            summary,
            UpgradeResult::DryRun,
            started,
            None,
            source_summary,
            installed_before_id,
            installed_before
                .as_ref()
                .map(|state| state.release_id.clone())
                .unwrap_or_default(),
            false,
            Vec::new(),
            None,
        ));
    }
    if matches!(verdict, Verdict::Noop) {
        let response = response_from_parts(
            source_manifest.release_id.clone(),
            summary,
            UpgradeResult::Committed,
            started,
            None,
            source_summary,
            installed_before_id,
            installed_before
                .as_ref()
                .map(|state| state.release_id.clone())
                .unwrap_or_else(|| source_manifest.release_id.clone()),
            false,
            Vec::new(),
            None,
        );
        append_release_history_best_effort(cfg, &response.release_history_entry).await;
        return Ok(response);
    }
    if matches!(verdict, Verdict::Breaking { .. }) && !req.allow_breaking {
        let response = response_from_parts(
            source_manifest.release_id.clone(),
            summary,
            UpgradeResult::Rejected,
            started,
            Some("breaking upgrade requires allowBreaking=true".into()),
            source_summary,
            installed_before_id.clone(),
            installed_before_id.unwrap_or_default(),
            false,
            Vec::new(),
            None,
        );
        append_release_history_best_effort(cfg, &response.release_history_entry).await;
        return Ok(response);
    }

    let mode = infer_package_mode_blocking(&package_dir).await?;
    let stage = stage_upgrade_blocking(cfg, &package_dir, mode).await?;
    let manifest = read_v2_manifest_blocking(&stage.stage_dir).await?;
    let verdict = stage
        .preflight
        .verdict
        .clone()
        .ok_or_else(|| ApplyError::bad_request("POST /upgrade/apply requires a v2 manifest"))?;

    match &verdict {
        Verdict::Noop => unreachable!("noop is returned before staging"),
        Verdict::Preserving {
            requires_db_backup, ..
        } => {
            apply_preserving(
                cfg,
                supervisor,
                &manifest,
                &verdict,
                *requires_db_backup,
                source_summary,
                installed_before_id,
                started,
            )
            .await
        }
        Verdict::Breaking { units_changed, .. } => {
            apply_breaking(
                cfg,
                supervisor,
                proc_supervisor,
                &manifest,
                &verdict,
                units_changed,
                source_summary,
                installed_before_id,
                started,
            )
            .await
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn apply_preserving(
    cfg: &AppConfig,
    supervisor: &Supervisor,
    manifest: &ReleaseManifestV2,
    verdict: &Verdict,
    requires_db_backup: bool,
    source: SourceSummary,
    installed_before_id: Option<String>,
    started: Instant,
) -> Result<UpgradeResponse, ApplyError> {
    let backup = if requires_db_backup {
        Some(backup_db(cfg, supervisor, &manifest.release_id).await?)
    } else {
        None
    };
    let plan =
        swap_symlinks_for_verdict_blocking(cfg, &stage_dir(cfg, &manifest.release_id), verdict)
            .await?;
    let mut restart_needed = false;
    let mut frontend_refresh = false;
    if let Verdict::Preserving { units_changed, .. } = verdict {
        for unit in units_changed {
            match manifest.units.get(unit).map(|unit| unit.restart_policy) {
                Some(RestartPolicy::RestartViaAdminApi) => restart_needed = true,
                Some(RestartPolicy::RefreshFrontend) => frontend_refresh = true,
                Some(RestartPolicy::DeferUntilFullReboot | RestartPolicy::NextSpawn) => {}
                Some(RestartPolicy::ExecSelfForBreakingOnly) | None => {}
            }
        }
    }

    if restart_needed {
        supervisor.restart().await?;
        supervisor.wait_for_spawn(Duration::from_secs(5)).await?;
        match healthcheck(cfg, supervisor, manifest).await {
            HealthcheckOutcome::Healthy => {}
            outcome => {
                let message = outcome.message();
                rollback_symlinks_blocking(&plan).await?;
                if let Some(backup) = &backup {
                    restore_db(cfg, supervisor, backup).await?;
                } else {
                    supervisor.restart().await?;
                    let _ = supervisor.wait_for_spawn(Duration::from_secs(5)).await;
                }
                let response = response_from_parts(
                    manifest.release_id.clone(),
                    VerdictSummary::from(verdict),
                    UpgradeResult::RolledBack,
                    started,
                    Some(message),
                    source,
                    installed_before_id.clone(),
                    installed_before_id.unwrap_or_default(),
                    false,
                    plan.changes,
                    backup,
                );
                append_release_history_best_effort(cfg, &response.release_history_entry).await;
                return Ok(response);
            }
        }
    }

    if frontend_refresh {
        write_last_upgrade_id_blocking(cfg, &manifest.release_id).await?;
    }
    let installed = InstalledState::from_manifest(manifest);
    write_installed_state_blocking(cfg, &installed).await?;
    let response = response_from_parts(
        manifest.release_id.clone(),
        VerdictSummary::from(verdict),
        UpgradeResult::Committed,
        started,
        None,
        source,
        installed_before_id.clone(),
        manifest.release_id.clone(),
        false,
        plan.changes,
        backup,
    );
    append_release_history_best_effort(cfg, &response.release_history_entry).await;
    Ok(response)
}

#[allow(clippy::too_many_arguments)]
async fn apply_breaking(
    cfg: &AppConfig,
    supervisor: &Supervisor,
    proc_supervisor: &Supervisor,
    manifest: &ReleaseManifestV2,
    verdict: &Verdict,
    units_changed: &[UnitName],
    source: SourceSummary,
    installed_before_id: Option<String>,
    started: Instant,
) -> Result<UpgradeResponse, ApplyError> {
    let backup = if units_changed.contains(&UnitName::CalmServer) {
        Some(backup_db(cfg, supervisor, &manifest.release_id).await?)
    } else {
        None
    };
    let plan = swap_all_symlinks_blocking(cfg, &stage_dir(cfg, &manifest.release_id)).await?;
    let response = response_from_parts(
        manifest.release_id.clone(),
        VerdictSummary::from(verdict),
        UpgradeResult::Committed,
        started,
        None,
        source.clone(),
        installed_before_id.clone(),
        manifest.release_id.clone(),
        true,
        plan.changes.clone(),
        backup.clone(),
    );
    let post_swap_result: anyhow::Result<()> = async {
        write_installed_state_blocking(cfg, &InstalledState::from_manifest(manifest)).await?;
        if units_changed.contains(&UnitName::CalmProcSupervisor) {
            proc_supervisor.stop_and_wait().await?;
        }
        append_release_history_checked(cfg, &response.release_history_entry).await?;
        Ok(())
    }
    .await;
    if let Err(err) = post_swap_result {
        let message = err.to_string();
        let mut rollback_error = None;
        if let Err(err) = rollback_symlinks_blocking(&plan).await {
            rollback_error = Some(format!("rollback symlinks failed: {err:#}"));
        } else if let Err(err) = write_installed_from_current_server_blocking(cfg).await {
            rollback_error = Some(format!("restore installed state failed: {err:#}"));
        }
        if let Some(backup) = &backup
            && let Err(err) = restore_db(cfg, supervisor, backup).await
        {
            rollback_error = Some(match rollback_error {
                Some(existing) => format!("{existing}; restore DB failed: {err:#}"),
                None => format!("restore DB failed: {err:#}"),
            });
        }
        if units_changed.contains(&UnitName::CalmProcSupervisor) {
            proc_supervisor.resume().await;
            let _ = proc_supervisor.wait_for_spawn(Duration::from_secs(5)).await;
        }
        let error = rollback_error
            .map(|rollback| format!("{message}; {rollback}"))
            .unwrap_or(message);
        let rolled_back = response_from_parts(
            manifest.release_id.clone(),
            VerdictSummary::from(verdict),
            UpgradeResult::RolledBack,
            started,
            Some(error.clone()),
            source,
            installed_before_id.clone(),
            installed_before_id.unwrap_or_default(),
            false,
            plan.changes.clone(),
            backup.clone(),
        );
        append_release_history_best_effort(cfg, &rolled_back.release_history_entry).await;
        return Err(ApplyError::internal(error));
    }
    Ok(response)
}

#[derive(Debug, Clone)]
struct SymlinkSwapPlan {
    changes: Vec<SymlinkPlanChange>,
}

async fn swap_symlinks_for_verdict_blocking(
    cfg: &AppConfig,
    stage_dir: &Path,
    verdict: &Verdict,
) -> anyhow::Result<SymlinkSwapPlan> {
    let cfg = cfg.clone();
    let stage_dir = stage_dir.to_path_buf();
    let verdict = verdict.clone();
    tokio::task::spawn_blocking(move || swap_symlinks_for_verdict(&cfg, &stage_dir, &verdict))
        .await
        .context("swap symlinks task panicked")?
}

async fn swap_all_symlinks_blocking(
    cfg: &AppConfig,
    stage_dir: &Path,
) -> anyhow::Result<SymlinkSwapPlan> {
    let cfg = cfg.clone();
    let stage_dir = stage_dir.to_path_buf();
    tokio::task::spawn_blocking(move || swap_all_symlinks(&cfg, &stage_dir))
        .await
        .context("swap all symlinks task panicked")?
}

fn swap_symlinks_for_verdict(
    cfg: &AppConfig,
    stage_dir: &Path,
    verdict: &Verdict,
) -> anyhow::Result<SymlinkSwapPlan> {
    let units = match verdict {
        Verdict::Noop => Vec::new(),
        Verdict::Preserving { units_changed, .. } | Verdict::Breaking { units_changed, .. } => {
            units_changed.clone()
        }
    };
    swap_symlink_roles(cfg, stage_dir, roles_for_units(&units)?)
}

fn swap_all_symlinks(cfg: &AppConfig, stage_dir: &Path) -> anyhow::Result<SymlinkSwapPlan> {
    swap_symlink_roles(cfg, stage_dir, [ReleaseRole::Server, ReleaseRole::Web])
}

fn swap_symlink_roles<I>(
    cfg: &AppConfig,
    stage_dir: &Path,
    roles: I,
) -> anyhow::Result<SymlinkSwapPlan>
where
    I: IntoIterator<Item = ReleaseRole>,
{
    upgrade::validate_staged_release_target(cfg, stage_dir)?;
    let mut seen = BTreeSet::new();
    let mut changes = Vec::new();
    for role in roles {
        if !seen.insert(role) {
            continue;
        }
        let pair = role.pair(cfg);
        let old_current = upgrade::read_symlink_if_exists(&pair.current)?
            .map(|target| upgrade::resolve_link_target(&pair.current, &target));
        let old_previous = upgrade::read_symlink_if_exists(&pair.previous)?
            .map(|target| upgrade::resolve_link_target(&pair.previous, &target));
        changes.push(SymlinkPlanChange {
            role: role.as_str().into(),
            current: pair.current,
            previous: pair.previous,
            old_current,
            old_previous,
            new_current: stage_dir.to_path_buf(),
        });
    }
    for change in &changes {
        if let Some(old_target) = &change.old_current {
            upgrade::replace_symlink_atomic(&change.previous, old_target)?;
        } else {
            upgrade::remove_symlink_if_exists(&change.previous)?;
        }
        upgrade::replace_symlink_atomic(&change.current, stage_dir)?;
    }
    Ok(SymlinkSwapPlan { changes })
}

fn rollback_symlinks(plan: &SymlinkSwapPlan) -> anyhow::Result<()> {
    for change in plan.changes.iter().rev() {
        if let Some(old_current) = &change.old_current {
            upgrade::replace_symlink_atomic(&change.current, old_current)?;
        } else {
            upgrade::remove_symlink_if_exists(&change.current)?;
        }
        if let Some(old_previous) = &change.old_previous {
            upgrade::replace_symlink_atomic(&change.previous, old_previous)?;
        } else {
            upgrade::remove_symlink_if_exists(&change.previous)?;
        }
    }
    Ok(())
}

async fn rollback_symlinks_blocking(plan: &SymlinkSwapPlan) -> anyhow::Result<()> {
    let plan = plan.clone();
    tokio::task::spawn_blocking(move || rollback_symlinks(&plan))
        .await
        .context("rollback symlinks task panicked")?
}

fn roles_for_units(units: &[UnitName]) -> anyhow::Result<Vec<ReleaseRole>> {
    let mut roles = Vec::new();
    for unit in units {
        match unit {
            UnitName::Web => roles.push(ReleaseRole::Web),
            UnitName::NeigeApp
            | UnitName::CalmServer
            | UnitName::CalmProcSupervisor
            | UnitName::NeigeCodexBridge
            | UnitName::NeigeMcpStdioShim
            | UnitName::NeigeCli => roles.push(ReleaseRole::Server),
        }
    }
    Ok(roles)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum ReleaseRole {
    Server,
    Web,
}

impl ReleaseRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Web => "web",
        }
    }

    fn pair(self, cfg: &AppConfig) -> SymlinkPair {
        match self {
            Self::Server => SymlinkPair {
                current: cfg.release.current_server.clone(),
                previous: cfg.release.previous_server.clone(),
            },
            Self::Web => SymlinkPair {
                current: cfg.release.current_web.clone(),
                previous: cfg.release.previous_web.clone(),
            },
        }
    }
}

struct SymlinkPair {
    current: PathBuf,
    previous: PathBuf,
}

async fn backup_db(
    cfg: &AppConfig,
    supervisor: &Supervisor,
    release_id: &str,
) -> anyhow::Result<PathBuf> {
    let db_url = cfg.child.db_url.as_ref().ok_or_else(|| {
        anyhow!("DB backup required but child.db_url is not configured as sqlite://path")
    })?;
    let db_path = upgrade::parse_sqlite_file_url(db_url)?;
    if !db_path.is_file() {
        return Err(anyhow!("SQLite DB {} does not exist", db_path.display()));
    }
    supervisor.stop_and_wait().await?;
    let backup_path = cfg
        .calm_data_dir_resolved()
        .join("backups")
        .join(release_id)
        .join("calm.db");
    let backup_result = backup_sqlite_files_blocking(&db_path, &backup_path).await;
    supervisor.resume().await;
    supervisor.wait_for_spawn(Duration::from_secs(5)).await?;
    backup_result?;
    Ok(backup_path)
}

async fn restore_db(
    cfg: &AppConfig,
    supervisor: &Supervisor,
    backup_path: &Path,
) -> anyhow::Result<()> {
    let db_url = cfg.child.db_url.as_ref().ok_or_else(|| {
        anyhow!("DB restore required but child.db_url is not configured as sqlite://path")
    })?;
    let db_path = upgrade::parse_sqlite_file_url(db_url)?;
    supervisor.stop_and_wait().await?;
    let restore_result = restore_sqlite_files_blocking(backup_path, &db_path).await;
    supervisor.resume().await;
    supervisor.wait_for_spawn(Duration::from_secs(5)).await?;
    restore_result?;
    Ok(())
}

async fn backup_sqlite_files_blocking(db_path: &Path, backup_path: &Path) -> anyhow::Result<()> {
    let db_path = db_path.to_path_buf();
    let backup_path = backup_path.to_path_buf();
    tokio::task::spawn_blocking(move || backup_sqlite_files_sync(&db_path, &backup_path))
        .await
        .context("backup DB task panicked")?
}

async fn restore_sqlite_files_blocking(backup_path: &Path, db_path: &Path) -> anyhow::Result<()> {
    let backup_path = backup_path.to_path_buf();
    let db_path = db_path.to_path_buf();
    tokio::task::spawn_blocking(move || restore_sqlite_files_sync(&backup_path, &db_path))
        .await
        .context("restore DB task panicked")?
}

fn backup_sqlite_files_sync(db_path: &Path, backup_path: &Path) -> anyhow::Result<()> {
    atomic_copy_file(db_path, backup_path)?;
    for suffix in ["wal", "shm"] {
        let sidecar = sqlite_sidecar_path(db_path, suffix);
        if sidecar.exists() {
            atomic_copy_file(&sidecar, &sqlite_sidecar_path(backup_path, suffix))?;
        }
    }
    Ok(())
}

fn restore_sqlite_files_sync(backup_path: &Path, db_path: &Path) -> anyhow::Result<()> {
    if !backup_path.is_file() {
        return Err(anyhow!(
            "SQLite backup {} does not exist",
            backup_path.display()
        ));
    }
    for suffix in ["wal", "shm"] {
        remove_file_if_exists(&sqlite_sidecar_path(db_path, suffix))?;
    }
    atomic_copy_file(backup_path, db_path)?;
    for suffix in ["wal", "shm"] {
        let backup_sidecar = sqlite_sidecar_path(backup_path, suffix);
        if backup_sidecar.exists() {
            atomic_copy_file(&backup_sidecar, &sqlite_sidecar_path(db_path, suffix))?;
        }
    }
    Ok(())
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}-{suffix}", path.display()))
}

fn remove_file_if_exists(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("remove {}", path.display())),
    }
}

fn atomic_copy_file(src: &Path, dst: &Path) -> anyhow::Result<()> {
    if let Some(parent) = dst.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let tmp = dst.with_extension(format!("tmp.{}", std::process::id()));
    if tmp.exists() {
        fs::remove_file(&tmp).with_context(|| format!("remove {}", tmp.display()))?;
    }
    {
        let mut src_file =
            fs::File::open(src).with_context(|| format!("open {}", src.display()))?;
        let mut dst_file =
            fs::File::create(&tmp).with_context(|| format!("create {}", tmp.display()))?;
        let mut buf = [0_u8; 64 * 1024];
        loop {
            let read = src_file.read(&mut buf)?;
            if read == 0 {
                break;
            }
            dst_file.write_all(&buf[..read])?;
        }
        dst_file.sync_all()?;
    }
    fs::rename(&tmp, dst)
        .with_context(|| format!("rename {} to {}", tmp.display(), dst.display()))?;
    Ok(())
}

#[derive(Debug)]
enum HealthcheckOutcome {
    Healthy,
    TimedOut { last_error: Option<String> },
    ProcessDied,
}

impl HealthcheckOutcome {
    fn message(&self) -> String {
        match self {
            Self::Healthy => "healthy".into(),
            Self::TimedOut { last_error } => last_error
                .clone()
                .unwrap_or_else(|| "healthcheck timed out".into()),
            Self::ProcessDied => "calm-server process died during healthcheck".into(),
        }
    }
}

async fn healthcheck(
    cfg: &AppConfig,
    supervisor: &Supervisor,
    manifest: &ReleaseManifestV2,
) -> HealthcheckOutcome {
    let target_version = manifest
        .units
        .get(&UnitName::CalmServer)
        .map(|unit| unit.version.clone());
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    let mut last_error = None;
    while tokio::time::Instant::now() < deadline {
        let status = supervisor.process_status().await;
        if status.child_state == "stopped" || status.child_pid.is_none() {
            return HealthcheckOutcome::ProcessDied;
        }
        match get_version(&cfg.child.calm_listen).await {
            Ok(version) if target_version.as_deref() == Some(version.as_str()) => {
                return HealthcheckOutcome::Healthy;
            }
            Ok(version) => {
                last_error = Some(format!("kernelVersion {version} did not match target"));
            }
            Err(err) => last_error = Some(err.to_string()),
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    HealthcheckOutcome::TimedOut { last_error }
}

async fn get_version(calm_listen: &str) -> anyhow::Result<String> {
    let addr: SocketAddr = calm_listen
        .parse()
        .with_context(|| format!("parse calm listen address {calm_listen}"))?;
    let mut stream = tokio::net::TcpStream::connect(addr).await?;
    stream
        .write_all(b"GET /api/version HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .await?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await?;
    let text = String::from_utf8_lossy(&buf);
    let (head, body) = text
        .split_once("\r\n\r\n")
        .ok_or_else(|| anyhow!("invalid HTTP response from /api/version"))?;
    if !head.starts_with("HTTP/1.1 200") && !head.starts_with("HTTP/1.0 200") {
        return Err(anyhow!("GET /api/version returned non-200"));
    }
    let value: serde_json::Value = serde_json::from_str(body.trim())?;
    value
        .get("kernelVersion")
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("GET /api/version response missing kernelVersion"))
}

fn merge_source_override(
    base: &SourceConfig,
    patch: Option<&SourceOverride>,
) -> Result<SourceConfig, ApplyError> {
    let mut source = base.clone();
    let Some(patch) = patch else {
        return Ok(source);
    };
    for (key, value) in &patch.0 {
        match key.as_str() {
            "type" => {
                if value.as_str() != Some("git") {
                    return Err(ApplyError::bad_request(
                        "source.type cannot change; only git sources are supported",
                    ));
                }
            }
            "url" => source.url = Some(string_field(key, value)?),
            "branch" | "ref" => source.branch = string_field(key, value)?,
            other => {
                return Err(ApplyError::bad_request(format!(
                    "unsupported source override field {other}"
                )));
            }
        }
    }
    Ok(source)
}

fn string_field(key: &str, value: &serde_json::Value) -> Result<String, ApplyError> {
    value
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| ApplyError::bad_request(format!("source.{key} must be a string")))
}

async fn read_v2_manifest_blocking(stage_dir: &Path) -> Result<ReleaseManifestV2, ApplyError> {
    let stage_dir = stage_dir.to_path_buf();
    tokio::task::spawn_blocking(move || read_v2_manifest(&stage_dir))
        .await
        .map_err(|err| ApplyError::internal(format!("read manifest task panicked: {err}")))?
}

fn read_v2_manifest(stage_dir: &Path) -> Result<ReleaseManifestV2, ApplyError> {
    match upgrade::read_versioned_manifest(stage_dir)? {
        VersionedReleaseManifest::V2(manifest) => Ok(manifest),
        VersionedReleaseManifest::V1(_) => Err(ApplyError::bad_request(
            "POST /upgrade/apply requires a manifest v2 package",
        )),
    }
}

async fn infer_package_mode_blocking(
    package_dir: &Path,
) -> anyhow::Result<preflight::PreflightMode> {
    let package_dir = package_dir.to_path_buf();
    tokio::task::spawn_blocking(move || upgrade::infer_package_mode(&package_dir))
        .await
        .context("infer package mode task panicked")?
}

async fn stage_upgrade_blocking(
    cfg: &AppConfig,
    package_dir: &Path,
    mode: preflight::PreflightMode,
) -> anyhow::Result<upgrade::UpgradeStageResult> {
    let cfg = cfg.clone();
    let package_dir = package_dir.to_path_buf();
    tokio::task::spawn_blocking(move || upgrade::stage_upgrade(&cfg, &package_dir, mode))
        .await
        .context("stage upgrade task panicked")?
}

async fn resolve_source_package_blocking(
    cfg: &AppConfig,
    source: &SourceConfig,
    allow_build: bool,
) -> Result<PathBuf, ApplyError> {
    let cfg = cfg.clone();
    let source = source.clone();
    tokio::task::spawn_blocking(move || resolve_source_package_sync(&cfg, &source, allow_build))
        .await
        .map_err(|err| ApplyError::internal(format!("source resolution task panicked: {err}")))?
}

fn resolve_source_package_sync(
    cfg: &AppConfig,
    source: &SourceConfig,
    allow_build: bool,
) -> Result<PathBuf, ApplyError> {
    if let Some(url) = &source.url {
        let path = PathBuf::from(url);
        if path.join("manifest.json").is_file() {
            return Ok(path);
        }
    }
    if !allow_build {
        return Err(ApplyError::bad_request(
            "dry-run requires source.url to point at a local manifest package; git-source dry-run would clone/build and is not zero-write",
        ));
    }
    source::build_source_package_from_source(cfg, source, None).map_err(ApplyError::from)
}

fn stage_dir(cfg: &AppConfig, release_id: &str) -> PathBuf {
    cfg.release.root.join("staged").join(release_id)
}

impl From<&Verdict> for VerdictSummary {
    fn from(value: &Verdict) -> Self {
        match value {
            Verdict::Noop => Self {
                kind: "noop".into(),
                units_changed: Vec::new(),
                deferred: Vec::new(),
                refresh_frontend: false,
                requires_db_backup: false,
                reason: None,
            },
            Verdict::Preserving {
                units_changed,
                deferred,
                refresh_frontend,
                requires_db_backup,
            } => Self {
                kind: "preserving".into(),
                units_changed: units_changed.clone(),
                deferred: deferred.clone(),
                refresh_frontend: *refresh_frontend,
                requires_db_backup: *requires_db_backup,
                reason: None,
            },
            Verdict::Breaking {
                reason,
                units_changed,
            } => Self {
                kind: "breaking".into(),
                units_changed: units_changed.clone(),
                deferred: Vec::new(),
                refresh_frontend: false,
                requires_db_backup: false,
                reason: Some(breaking_reason(reason).into()),
            },
        }
    }
}

fn breaking_reason(reason: &BreakingReason) -> &'static str {
    match reason {
        BreakingReason::ProductMajorChanged => "productMajorChanged",
        BreakingReason::WireIncompatibility => "wireIncompatibility",
        BreakingReason::DestructiveDbMigration => "destructiveDbMigration",
        BreakingReason::NoInstalledState => "noInstalledState",
    }
}

#[allow(clippy::too_many_arguments)]
fn response_from_parts(
    release_id: String,
    verdict: VerdictSummary,
    result: UpgradeResult,
    started: Instant,
    error: Option<String>,
    source: SourceSummary,
    installed_at_before: Option<String>,
    installed_at_after: String,
    executed_breaking_self_exec: bool,
    symlink_changes: Vec<SymlinkPlanChange>,
    db_backup: Option<PathBuf>,
) -> UpgradeResponse {
    let duration_ms = started.elapsed().as_millis().try_into().unwrap_or(u64::MAX);
    let result_text = result_text(result).to_string();
    let entry = ReleaseHistoryEntry {
        kind: default_history_kind(),
        release_id: release_id.clone(),
        timestamp: chrono::Utc::now().to_rfc3339(),
        verdict_kind: verdict.kind.clone(),
        verdict_reason: verdict.reason.clone(),
        units_changed: verdict.units_changed.clone(),
        deferred: verdict.deferred.clone(),
        refresh_frontend: verdict.refresh_frontend,
        requires_db_backup: verdict.requires_db_backup,
        result: result_text,
        duration_ms,
        error: error.clone(),
        source,
        installed_at_before,
        installed_at_after,
        executed_breaking_self_exec,
        symlink_changes,
        db_backup,
    };
    UpgradeResponse {
        release_id,
        units_changed: verdict.units_changed.clone(),
        deferred: verdict.deferred.clone(),
        verdict,
        result,
        duration_ms,
        error,
        release_history_entry: entry,
    }
}

fn default_history_kind() -> String {
    "apply".into()
}

fn result_text(result: UpgradeResult) -> &'static str {
    match result {
        UpgradeResult::Committed => "committed",
        UpgradeResult::RolledBack => "rolledBack",
        UpgradeResult::Rejected => "rejected",
        UpgradeResult::DryRun => "dryRun",
    }
}

async fn append_release_history_best_effort(cfg: &AppConfig, entry: &ReleaseHistoryEntry) {
    if let Err(err) = append_release_history_checked(cfg, entry).await {
        tracing::error!(error = %err, "failed to append release history");
    }
}

async fn append_release_history_checked(
    cfg: &AppConfig,
    entry: &ReleaseHistoryEntry,
) -> anyhow::Result<()> {
    let cfg = cfg.clone();
    let entry = entry.clone();
    tokio::task::spawn_blocking(move || append_release_history_sync(&cfg, &entry))
        .await
        .context("append release history task panicked")?
}

fn append_release_history_sync(cfg: &AppConfig, entry: &ReleaseHistoryEntry) -> anyhow::Result<()> {
    let path = release_history_path(cfg);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    writeln!(file, "{}", serde_json::to_string(entry)?)?;
    file.sync_all()?;
    Ok(())
}

pub(crate) async fn read_release_history_blocking(
    cfg: &AppConfig,
    limit: usize,
) -> anyhow::Result<Vec<ReleaseHistoryEntry>> {
    let cfg = cfg.clone();
    tokio::task::spawn_blocking(move || read_release_history(&cfg, limit))
        .await
        .context("read release history task panicked")?
}

pub(crate) fn read_release_history(
    cfg: &AppConfig,
    limit: usize,
) -> anyhow::Result<Vec<ReleaseHistoryEntry>> {
    let path = release_history_path(cfg);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => return Err(err).with_context(|| format!("read {}", path.display())),
    };
    let mut entries = Vec::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        entries.push(serde_json::from_str(line)?);
    }
    let start = entries.len().saturating_sub(limit);
    Ok(entries.split_off(start))
}

pub(crate) async fn rollback_last_preserving(
    cfg: &AppConfig,
    supervisor: &Supervisor,
    to: &str,
) -> Result<UpgradeResponse, ApplyError> {
    let started = Instant::now();
    let history = read_release_history_blocking(cfg, usize::MAX).await?;
    let Some(last) = history
        .into_iter()
        .rev()
        .find(|entry| entry.result == "committed" && !is_rollback_history(entry))
    else {
        return Err(ApplyError::bad_request("release history is empty"));
    };
    if last.verdict_kind != "preserving" {
        return Err(ApplyError::invalid_rollback_target(
            "rollback only supports the last committed preserving apply",
        ));
    }
    if last.installed_at_before.as_deref() != Some(to) {
        return Err(ApplyError::invalid_rollback_target(
            "rollback target must be the release immediately before the last apply",
        ));
    }
    let installed = read_installed_state_blocking(cfg).await?;
    let current_release_id = installed.as_ref().map(|state| state.release_id.as_str());
    if current_release_id != Some(last.release_id.as_str()) {
        return Err(ApplyError::invalid_rollback_target(
            "current installed release does not match the last committed apply",
        ));
    }
    if let Some(backup) = &last.db_backup
        && !backup.is_file()
    {
        return Err(ApplyError {
            status: StatusCode::CONFLICT,
            code: "backup_missing",
            message: format!("rollback backup {} is missing", backup.display()),
        });
    }
    let plan = SymlinkSwapPlan {
        changes: last.symlink_changes.clone(),
    };
    rollback_symlinks_blocking(&plan).await?;
    if let Some(backup) = &last.db_backup {
        restore_db(cfg, supervisor, backup).await?;
    } else if last.units_changed.contains(&UnitName::CalmServer) {
        supervisor.restart().await?;
        supervisor.wait_for_spawn(Duration::from_secs(5)).await?;
    }
    write_installed_from_current_server_blocking(cfg).await?;
    let installed_after = read_installed_state_blocking(cfg)
        .await?
        .map(|state| state.release_id)
        .unwrap_or_else(|| to.to_string());
    let source = last.source.clone();
    let verdict = VerdictSummary {
        kind: "rollback".into(),
        units_changed: last.units_changed.clone(),
        deferred: last.deferred.clone(),
        refresh_frontend: last.refresh_frontend,
        requires_db_backup: last.requires_db_backup,
        reason: None,
    };
    let mut response = response_from_parts(
        to.to_string(),
        verdict,
        UpgradeResult::Committed,
        started,
        None,
        source,
        Some(last.release_id),
        installed_after,
        false,
        plan.changes,
        last.db_backup,
    );
    response.release_history_entry.kind = "rollback".into();
    append_release_history_best_effort(cfg, &response.release_history_entry).await;
    Ok(response)
}

fn is_rollback_history(entry: &ReleaseHistoryEntry) -> bool {
    entry.kind == "rollback" || entry.verdict_kind == "rollback"
}

fn write_installed_from_current_server(cfg: &AppConfig) -> anyhow::Result<()> {
    let target = fs::read_link(&cfg.release.current_server)
        .with_context(|| format!("read {}", cfg.release.current_server.display()))?;
    let target = upgrade::resolve_link_target(&cfg.release.current_server, &target);
    if let VersionedReleaseManifest::V2(manifest) = upgrade::read_versioned_manifest(&target)? {
        write_installed_state(
            &cfg.calm_data_dir_resolved(),
            &InstalledState::from_manifest(&manifest),
        )?;
    }
    Ok(())
}

async fn write_installed_from_current_server_blocking(cfg: &AppConfig) -> anyhow::Result<()> {
    let cfg = cfg.clone();
    tokio::task::spawn_blocking(move || write_installed_from_current_server(&cfg))
        .await
        .context("write installed from current server task panicked")?
}

async fn read_installed_state_blocking(cfg: &AppConfig) -> anyhow::Result<Option<InstalledState>> {
    let data_dir = cfg.calm_data_dir_resolved();
    tokio::task::spawn_blocking(move || read_installed_state(&data_dir))
        .await
        .context("read installed state task panicked")?
}

async fn write_installed_state_blocking(
    cfg: &AppConfig,
    installed: &InstalledState,
) -> anyhow::Result<()> {
    let data_dir = cfg.calm_data_dir_resolved();
    let installed = installed.clone();
    tokio::task::spawn_blocking(move || write_installed_state(&data_dir, &installed))
        .await
        .context("write installed state task panicked")?
}

pub(crate) fn release_history_path(cfg: &AppConfig) -> PathBuf {
    cfg.calm_data_dir_resolved()
        .join("state")
        .join("release-history.jsonl")
}

pub(crate) fn last_upgrade_id_path(cfg: &AppConfig) -> PathBuf {
    cfg.calm_data_dir_resolved()
        .join("state")
        .join("last-upgrade-id")
}

pub(crate) async fn read_last_upgrade_id_blocking(
    cfg: &AppConfig,
) -> anyhow::Result<Option<String>> {
    let path = last_upgrade_id_path(cfg);
    tokio::task::spawn_blocking(move || match fs::read_to_string(&path) {
        Ok(value) => Ok(Some(value.trim().to_string())),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(anyhow::Error::from(err).context("read last upgrade id")),
    })
    .await
    .context("read last upgrade id task panicked")?
}

fn write_last_upgrade_id(cfg: &AppConfig, release_id: &str) -> anyhow::Result<()> {
    let path = last_upgrade_id_path(cfg);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(&path, release_id).with_context(|| format!("write {}", path.display()))
}

async fn write_last_upgrade_id_blocking(cfg: &AppConfig, release_id: &str) -> anyhow::Result<()> {
    let cfg = cfg.clone();
    let release_id = release_id.to_string();
    tokio::task::spawn_blocking(move || write_last_upgrade_id(&cfg, &release_id))
        .await
        .context("write last upgrade id task panicked")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn sqlite_backup_restore_copies_wal_and_shm() {
        let tmp = test_temp_dir("sqlite-backup-restore");
        let db = tmp.join("calm.db");
        let backup = tmp.join("backup").join("calm.db");
        fs::write(&db, "main-v1").expect("write db");
        fs::write(sqlite_sidecar_path(&db, "wal"), "wal-v1").expect("write wal");
        fs::write(sqlite_sidecar_path(&db, "shm"), "shm-v1").expect("write shm");

        backup_sqlite_files_sync(&db, &backup).expect("backup sqlite files");
        fs::write(&db, "main-v2").expect("overwrite db");
        fs::write(sqlite_sidecar_path(&db, "wal"), "wal-v2").expect("overwrite wal");
        fs::write(sqlite_sidecar_path(&db, "shm"), "shm-v2").expect("overwrite shm");

        restore_sqlite_files_sync(&backup, &db).expect("restore sqlite files");

        assert_eq!(fs::read_to_string(&db).expect("read db"), "main-v1");
        assert_eq!(
            fs::read_to_string(sqlite_sidecar_path(&db, "wal")).expect("read wal"),
            "wal-v1"
        );
        assert_eq!(
            fs::read_to_string(sqlite_sidecar_path(&db, "shm")).expect("read shm"),
            "shm-v1"
        );
    }

    fn test_temp_dir(name: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock before Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "neige-app-apply-{name}-{}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("create temp dir");
        path
    }
}
