use std::collections::HashSet;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

use crate::installed::{InstalledState, InstalledUnit};
use crate::manifest::{
    BundleUnit, CalmServerUnit, Compatibility, CompatibilityV1, CurrentVersion, DbMigrationPolicy,
    FileManifest, FileUnit, ReleaseManifest, ReleaseManifestV2, RestartPolicy, UnitName,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum PreflightMode {
    WebOnly,
    ServerOnly,
    Bundle,
    AppOnly,
}

impl PreflightMode {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            PreflightMode::WebOnly => "web-only",
            PreflightMode::ServerOnly => "server-only",
            PreflightMode::Bundle => "bundle",
            PreflightMode::AppOnly => "app-only",
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct PreflightResult {
    pub allowed: bool,
    pub mode: String,
    pub requires_db_backup: bool,
    pub reason: String,
    pub required_action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verdict: Option<Verdict>,
}

impl PreflightResult {
    fn allow(mode: PreflightMode, requires_db_backup: bool, reason: impl Into<String>) -> Self {
        Self {
            allowed: true,
            mode: mode.as_str().to_string(),
            requires_db_backup,
            reason: reason.into(),
            required_action: if requires_db_backup {
                "backup-db-before-activate".to_string()
            } else {
                "none".to_string()
            },
            verdict: None,
        }
    }

    pub(crate) fn deny(
        mode: PreflightMode,
        reason: impl Into<String>,
        action: impl Into<String>,
    ) -> Self {
        Self {
            allowed: false,
            mode: mode.as_str().to_string(),
            requires_db_backup: false,
            reason: reason.into(),
            required_action: action.into(),
            verdict: None,
        }
    }

    pub(crate) fn from_verdict(mode: PreflightMode, verdict: Verdict) -> Self {
        match &verdict {
            Verdict::Noop => Self {
                allowed: true,
                mode: mode.as_str().to_string(),
                requires_db_backup: false,
                reason: "target release is already installed".into(),
                required_action: "none".into(),
                verdict: Some(verdict),
            },
            Verdict::Preserving {
                requires_db_backup,
                refresh_frontend,
                deferred,
                ..
            } => Self {
                allowed: true,
                mode: mode.as_str().to_string(),
                requires_db_backup: *requires_db_backup,
                reason: "target release is preserving for this installed state".into(),
                required_action: preserving_action(
                    *requires_db_backup,
                    *refresh_frontend,
                    deferred,
                ),
                verdict: Some(verdict),
            },
            Verdict::Breaking { reason, .. } => Self {
                allowed: false,
                mode: mode.as_str().to_string(),
                requires_db_backup: false,
                reason: format!("breaking upgrade requires explicit opt-in: {reason:?}"),
                required_action: "allow-breaking-upgrade".into(),
                verdict: Some(verdict),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub(crate) enum Verdict {
    Noop,
    Preserving {
        units_changed: Vec<UnitName>,
        deferred: Vec<UnitName>,
        refresh_frontend: bool,
        requires_db_backup: bool,
    },
    Breaking {
        reason: BreakingReason,
        units_changed: Vec<UnitName>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum BreakingReason {
    ProductMajorChanged,
    WireIncompatibility,
    DestructiveDbMigration,
    NoInstalledState,
}

fn preserving_action(
    requires_db_backup: bool,
    refresh_frontend: bool,
    deferred: &[UnitName],
) -> String {
    if requires_db_backup {
        "backup-db-before-activate".into()
    } else if refresh_frontend {
        "activate-staged-release-and-refresh-frontend".into()
    } else if !deferred.is_empty() {
        "activate-staged-release-deferred-until-full-reboot".into()
    } else {
        "activate-staged-release".into()
    }
}

pub(crate) fn run_preflight(
    mode: PreflightMode,
    current: &CurrentVersion,
    manifest: &ReleaseManifest,
) -> PreflightResult {
    if manifest.schema_version != 1 {
        return PreflightResult::deny(
            mode,
            format!(
                "unsupported manifest schemaVersion {}",
                manifest.schema_version
            ),
            "provide-schema-version-1-manifest",
        );
    }

    match mode {
        PreflightMode::WebOnly => preflight_web_only(current, manifest),
        PreflightMode::ServerOnly => preflight_server_only(current, manifest),
        PreflightMode::Bundle => preflight_bundle(manifest),
        PreflightMode::AppOnly => preflight_app_only(manifest),
    }
}

pub(crate) fn run_preflight_v2(
    installed: Option<&InstalledState>,
    manifest: &ReleaseManifestV2,
) -> Verdict {
    let Some(installed) = installed else {
        return Verdict::Breaking {
            reason: BreakingReason::NoInstalledState,
            units_changed: manifest.units.keys().copied().collect(),
        };
    };
    compute_verdict(installed, manifest)
}

pub(crate) fn compute_verdict(installed: &InstalledState, target: &ReleaseManifestV2) -> Verdict {
    let units_changed = changed_units(installed, target);
    if target.product_major != installed.product_major {
        return Verdict::Breaking {
            reason: BreakingReason::ProductMajorChanged,
            units_changed,
        };
    }
    if compatibility_breaks(&installed.compatibility, &target.compatibility) {
        return Verdict::Breaking {
            reason: BreakingReason::WireIncompatibility,
            units_changed,
        };
    }
    if target
        .units
        .values()
        .any(|unit| unit.db_migration_policy == Some(DbMigrationPolicy::Destructive))
    {
        return Verdict::Breaking {
            reason: BreakingReason::DestructiveDbMigration,
            units_changed,
        };
    }
    if units_changed.is_empty() {
        return Verdict::Noop;
    }

    let deferred = units_changed
        .iter()
        .copied()
        .filter(|name| {
            target
                .units
                .get(name)
                .map(|unit| unit.restart_policy == RestartPolicy::DeferUntilFullReboot)
                .unwrap_or(false)
        })
        .collect();
    let refresh_frontend = units_changed.iter().any(|name| {
        target
            .units
            .get(name)
            .map(|unit| unit.restart_policy == RestartPolicy::RefreshFrontend)
            .unwrap_or(false)
    });
    let requires_db_backup = units_changed.contains(&UnitName::CalmServer)
        && matches!(
            target
                .units
                .get(&UnitName::CalmServer)
                .and_then(|unit| unit.db_migration_policy),
            Some(DbMigrationPolicy::Additive | DbMigrationPolicy::ForwardOnly)
        );

    Verdict::Preserving {
        units_changed,
        deferred,
        refresh_frontend,
        requires_db_backup,
    }
}

fn changed_units(installed: &InstalledState, target: &ReleaseManifestV2) -> Vec<UnitName> {
    target
        .units
        .iter()
        .filter_map(|(name, target_unit)| {
            let installed_unit = installed.units.get(name);
            let target_as_installed = InstalledUnit {
                version: target_unit.version.clone(),
                binary_sha256: target_unit.binary_sha256.clone(),
                tree_sha256: target_unit.tree_sha256.clone(),
            };
            if installed_unit == Some(&target_as_installed) {
                None
            } else {
                Some(*name)
            }
        })
        .collect()
}

fn compatibility_breaks(installed: &Compatibility, target: &Compatibility) -> bool {
    installed.terminal_frame_version != target.terminal_frame_version
        || installed.terminal_protocol_version != target.terminal_protocol_version
        || installed.api_version != target.api_version
        || installed.sync_event_version != target.sync_event_version
        || installed.mcp_protocol_version != target.mcp_protocol_version
        || installed.plugin_mcp_protocol_version != target.plugin_mcp_protocol_version
        || installed.supervisor_control_version != target.supervisor_control_version
        || target.min_web_compat_version > installed.web_compat_version
}

pub(crate) fn infer_mode(manifest: &ReleaseManifest) -> Result<PreflightMode, String> {
    let has_app = manifest.units.app.is_some();
    let has_web = manifest.units.web.is_some();
    let has_server = manifest.units.calm_server.is_some();
    let has_bundle = manifest.units.bundle.is_some();

    match (has_app, has_web, has_server, has_bundle) {
        (true, false, false, false) => Ok(PreflightMode::AppOnly),
        (false, true, false, false) => Ok(PreflightMode::WebOnly),
        (false, false, true, false) | (false, false, true, true) => Ok(PreflightMode::ServerOnly),
        (_, true, true, true) => Ok(PreflightMode::Bundle),
        _ => Err("unable to infer upgrade mode from manifest units".into()),
    }
}

fn preflight_web_only(current: &CurrentVersion, manifest: &ReleaseManifest) -> PreflightResult {
    let mode = PreflightMode::WebOnly;
    let Some(web) = manifest.units.web.as_ref() else {
        return PreflightResult::deny(mode, "manifest is missing units.web", "provide-web-unit");
    };
    if let Err(reason) = require_web_files(manifest) {
        return PreflightResult::deny(mode, reason, "provide-web-files");
    }
    let target = &web.compatibility;

    if let Some(reason) = protocols_mismatch_current(current, target) {
        return PreflightResult::deny(mode, reason, "ship-matching-web-or-bundle");
    }
    if target.web_compat_version < current.min_web_compat_version {
        return PreflightResult::deny(
            mode,
            format!(
                "target webCompatVersion {} is below current server minWebCompatVersion {}",
                target.web_compat_version, current.min_web_compat_version
            ),
            "ship-newer-web",
        );
    }

    PreflightResult::allow(mode, false, "target web is compatible with current server")
}

fn preflight_server_only(current: &CurrentVersion, manifest: &ReleaseManifest) -> PreflightResult {
    let mode = PreflightMode::ServerOnly;
    let Some(server) = manifest.units.calm_server.as_ref() else {
        return PreflightResult::deny(
            mode,
            "manifest is missing units.calmServer",
            "provide-calm-server-unit",
        );
    };
    let Some(bundle) = manifest.units.bundle.as_ref() else {
        return PreflightResult::deny(
            mode,
            "manifest is missing units.bundle for backend sidecars",
            "provide-backend-bundle-unit",
        );
    };
    if let Err(reason) = require_bundle_files(manifest, bundle) {
        return PreflightResult::deny(mode, reason, "provide-backend-bundle-files");
    }
    let target = &server.compatibility;

    if let Some(reason) = protocols_mismatch_current(current, target) {
        return PreflightResult::deny(mode, reason, "ship-matching-server-or-bundle");
    }

    let current_web_compat = current.conservative_web_compat_version();
    if current_web_compat < target.min_web_compat_version {
        return PreflightResult::deny(
            mode,
            format!(
                "current web compatibility {} is below target server minWebCompatVersion {}",
                current_web_compat, target.min_web_compat_version
            ),
            "upgrade-web-or-ship-bundle",
        );
    }

    db_policy_result(mode, server, "target server is compatible with current web")
}

fn preflight_bundle(manifest: &ReleaseManifest) -> PreflightResult {
    let mode = PreflightMode::Bundle;
    let Some(web) = manifest.units.web.as_ref() else {
        return PreflightResult::deny(mode, "manifest is missing units.web", "provide-web-unit");
    };
    let Some(server) = manifest.units.calm_server.as_ref() else {
        return PreflightResult::deny(
            mode,
            "manifest is missing units.calmServer",
            "provide-calm-server-unit",
        );
    };
    let Some(bundle) = manifest.units.bundle.as_ref() else {
        return PreflightResult::deny(
            mode,
            "manifest is missing units.bundle",
            "provide-bundle-unit",
        );
    };
    if let Err(reason) = require_web_files(manifest) {
        return PreflightResult::deny(mode, reason, "provide-web-files");
    }
    if let Err(reason) = require_bundle_files(manifest, bundle) {
        return PreflightResult::deny(mode, reason, "provide-bundle-files");
    }

    let web_compat = &web.compatibility;
    let server_compat = &server.compatibility;
    if web_compat.api_version != server_compat.api_version {
        return PreflightResult::deny(
            mode,
            "bundle web apiVersion does not match server apiVersion",
            "ship-internally-compatible-bundle",
        );
    }
    if web_compat.sync_event_version != server_compat.sync_event_version {
        return PreflightResult::deny(
            mode,
            "bundle web syncEventVersion does not match server syncEventVersion",
            "ship-internally-compatible-bundle",
        );
    }
    if web_compat.mcp_protocol_version != server_compat.mcp_protocol_version {
        return PreflightResult::deny(
            mode,
            "bundle web mcpProtocolVersion does not match server mcpProtocolVersion",
            "ship-internally-compatible-bundle",
        );
    }
    if web_compat.web_compat_version < server_compat.min_web_compat_version {
        return PreflightResult::deny(
            mode,
            format!(
                "bundle webCompatVersion {} is below bundled server minWebCompatVersion {}",
                web_compat.web_compat_version, server_compat.min_web_compat_version
            ),
            "ship-newer-web-in-bundle",
        );
    }

    db_policy_result(mode, server, "bundle is internally compatible")
}

fn preflight_app_only(manifest: &ReleaseManifest) -> PreflightResult {
    let mode = PreflightMode::AppOnly;
    let Some(app) = manifest.units.app.as_ref() else {
        return PreflightResult::deny(mode, "manifest is missing units.app", "provide-app-unit");
    };
    if app.name != "neige-app" {
        return PreflightResult::deny(
            mode,
            format!("app-only manifest targets {}", app.name),
            "provide-neige-app-unit",
        );
    }
    if let Err(reason) = require_file(manifest, FileUnit::App, "bin/neige-app") {
        return PreflightResult::deny(mode, reason, "provide-app-file");
    }

    PreflightResult::allow(mode, false, "target app manifest exists")
}

fn require_file(
    manifest: &ReleaseManifest,
    unit: FileUnit,
    expected_path: &str,
) -> Result<(), String> {
    let Some(file) = manifest
        .files
        .iter()
        .find(|file| file.unit == unit && file.path == expected_path)
    else {
        return Err(format!(
            "manifest files missing unit={unit:?} path {expected_path}"
        ));
    };
    validate_file(file)
}

fn require_web_files(manifest: &ReleaseManifest) -> Result<(), String> {
    let mut count = 0;
    for file in manifest
        .files
        .iter()
        .filter(|file| file.unit == FileUnit::Web && file.path.starts_with("web/dist/"))
    {
        validate_file(file)?;
        count += 1;
    }
    if count == 0 {
        return Err("manifest files missing web files under web/dist/".into());
    }
    Ok(())
}

fn require_bundle_files(manifest: &ReleaseManifest, bundle: &BundleUnit) -> Result<(), String> {
    let required = [
        "calm-server",
        "neige-codex-bridge",
        "neige-mcp-stdio-shim",
        "neige",
    ];
    let mut required_paths = HashSet::new();
    for name in required {
        let Some(binary) = bundle
            .binaries
            .iter()
            .find(|binary| binary.name.as_str() == name)
        else {
            return Err(format!("bundle binaries missing required binary {name}"));
        };

        let expected_path = format!("bin/{name}");
        if binary.path != expected_path {
            return Err(format!(
                "bundle binary {name} must map to {expected_path}, got {}",
                binary.path
            ));
        }
        if !required_paths.insert(binary.path.as_str()) {
            return Err(format!("bundle binary path {} is duplicated", binary.path));
        }

        let expected_unit = if name == "calm-server" {
            FileUnit::CalmServer
        } else {
            FileUnit::Bundle
        };
        let Some(file) = manifest
            .files
            .iter()
            .find(|file| file.path == binary.path && file.unit == expected_unit)
        else {
            return Err(format!(
                "manifest files missing bundle binary {name} at {} with unit {expected_unit:?}",
                binary.path
            ));
        };
        validate_file(file)?;
    }

    let all_paths: HashSet<&str> = bundle
        .binaries
        .iter()
        .map(|binary| binary.path.as_str())
        .collect();
    if all_paths.len() != bundle.binaries.len() {
        return Err("bundle binary paths must be distinct".into());
    }

    for binary in &bundle.binaries {
        if required_paths.contains(binary.path.as_str()) {
            continue;
        }
        let Some(file) = manifest.files.iter().find(|file| file.path == binary.path) else {
            return Err(format!(
                "manifest files missing bundle binary {} at {}",
                binary.name, binary.path
            ));
        };
        validate_file(file)?;
    }
    Ok(())
}

fn validate_file(file: &FileManifest) -> Result<(), String> {
    if file.bytes == 0 {
        return Err(format!("manifest file {} has zero bytes", file.path));
    }
    if file.sha256.len() != 64 || !file.sha256.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(format!("manifest file {} has invalid sha256", file.path));
    }
    Ok(())
}

fn protocols_mismatch_current(
    current: &CurrentVersion,
    target: &CompatibilityV1,
) -> Option<String> {
    if current.api_version != target.api_version {
        return Some(format!(
            "apiVersion mismatch: current {}, target {}",
            current.api_version, target.api_version
        ));
    }
    if current.sync_event_version != target.sync_event_version {
        return Some(format!(
            "syncEventVersion mismatch: current {}, target {}",
            current.sync_event_version, target.sync_event_version
        ));
    }
    if current.mcp_protocol_version != target.mcp_protocol_version {
        return Some(format!(
            "mcpProtocolVersion mismatch: current {}, target {}",
            current.mcp_protocol_version, target.mcp_protocol_version
        ));
    }
    None
}

fn db_policy_result(
    mode: PreflightMode,
    server: &CalmServerUnit,
    allow_reason: &'static str,
) -> PreflightResult {
    match server.db_migration_policy {
        DbMigrationPolicy::None => PreflightResult::allow(mode, false, allow_reason),
        DbMigrationPolicy::Additive => {
            PreflightResult::allow(mode, true, "additive DB migration requires backup")
        }
        DbMigrationPolicy::ForwardOnly => {
            PreflightResult::allow(mode, true, "forward-only DB migration requires backup")
        }
        DbMigrationPolicy::Destructive => PreflightResult::deny(
            mode,
            "destructive DB migration policy is not allowed for automatic preflight",
            "manual-migration-required",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    use crate::manifest::{
        AppUnit, BinaryUnit, BundleUnit, FileManifest, ReleaseUnit, ReleaseUnits, RestartPolicy,
        WebUnit,
    };

    fn compat(web: u32, min_web: u32) -> CompatibilityV1 {
        CompatibilityV1 {
            api_version: "1".into(),
            sync_event_version: 1,
            mcp_protocol_version: "2025-11-25".into(),
            web_compat_version: web,
            min_web_compat_version: min_web,
        }
    }

    fn current() -> CurrentVersion {
        CurrentVersion {
            api_version: "1".into(),
            sync_event_version: 1,
            mcp_protocol_version: "2025-11-25".into(),
            min_web_compat_version: 2,
            web_compat_version: Some(2),
            plugin_mcp_protocol_version: None,
            supervisor_control_version: None,
        }
    }

    fn manifest(units: ReleaseUnits, files: Vec<FileManifest>) -> ReleaseManifest {
        ReleaseManifest {
            schema_version: 1,
            release_id: "test".into(),
            units,
            files,
        }
    }

    fn server(policy: DbMigrationPolicy, min_web: u32) -> CalmServerUnit {
        CalmServerUnit {
            version: "0.1.0".into(),
            compatibility: compat(2, min_web),
            db_migration_policy: policy,
        }
    }

    fn file(unit: FileUnit, path: &str) -> FileManifest {
        FileManifest {
            path: path.into(),
            sha256: "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".into(),
            bytes: 1,
            unit,
        }
    }

    fn web_file() -> FileManifest {
        file(FileUnit::Web, "web/dist/index.html")
    }

    fn server_file() -> FileManifest {
        file(FileUnit::CalmServer, "bin/calm-server")
    }

    fn app_file() -> FileManifest {
        file(FileUnit::App, "bin/neige-app")
    }

    fn compat_v2(web: u32, min_web: u32) -> Compatibility {
        Compatibility {
            terminal_frame_version: 4,
            terminal_protocol_version: 4,
            api_version: "1".into(),
            sync_event_version: 1,
            mcp_protocol_version: "2024-11-05".into(),
            plugin_mcp_protocol_version: "2025-11-25".into(),
            web_compat_version: web,
            min_web_compat_version: min_web,
            supervisor_control_version: 1,
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

    fn v2_manifest(units: BTreeMap<UnitName, ReleaseUnit>) -> ReleaseManifestV2 {
        ReleaseManifestV2 {
            schema_version: 2,
            release_id: "target".into(),
            product_major: 0,
            compatibility: compat_v2(2, 2),
            units,
            files: Vec::new(),
        }
    }

    fn installed_from(target: &ReleaseManifestV2) -> InstalledState {
        InstalledState::from_manifest(target)
    }

    fn v2_units() -> BTreeMap<UnitName, ReleaseUnit> {
        let mut units = BTreeMap::new();
        units.insert(
            UnitName::CalmServer,
            release_unit(
                "0.1.0",
                Some(&"a".repeat(64)),
                None,
                RestartPolicy::RestartViaAdminApi,
                Some(DbMigrationPolicy::None),
            ),
        );
        units.insert(
            UnitName::CalmProcSupervisor,
            release_unit(
                "0.1.0",
                Some(&"b".repeat(64)),
                None,
                RestartPolicy::DeferUntilFullReboot,
                None,
            ),
        );
        units.insert(
            UnitName::Web,
            release_unit(
                "0.1.0",
                None,
                Some(&"c".repeat(64)),
                RestartPolicy::RefreshFrontend,
                None,
            ),
        );
        units
    }

    fn bundle_binaries() -> Vec<BinaryUnit> {
        [
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
        .collect()
    }

    fn bundle_files() -> Vec<FileManifest> {
        vec![
            server_file(),
            file(FileUnit::Bundle, "bin/neige-codex-bridge"),
            file(FileUnit::Bundle, "bin/neige-mcp-stdio-shim"),
            file(FileUnit::Bundle, "bin/neige"),
            web_file(),
        ]
    }

    fn backend_files() -> Vec<FileManifest> {
        bundle_files()
            .into_iter()
            .filter(|file| file.unit != FileUnit::Web)
            .collect()
    }

    #[test]
    fn web_only_allows_compatible_web() {
        let result = run_preflight(
            PreflightMode::WebOnly,
            &current(),
            &manifest(
                ReleaseUnits {
                    web: Some(WebUnit {
                        version: "web".into(),
                        compatibility: compat(2, 2),
                    }),
                    ..ReleaseUnits::default()
                },
                vec![web_file()],
            ),
        );

        assert!(result.allowed);
        assert!(!result.requires_db_backup);
    }

    #[test]
    fn web_only_denies_old_web_compat() {
        let result = run_preflight(
            PreflightMode::WebOnly,
            &current(),
            &manifest(
                ReleaseUnits {
                    web: Some(WebUnit {
                        version: "web".into(),
                        compatibility: compat(1, 1),
                    }),
                    ..ReleaseUnits::default()
                },
                vec![web_file()],
            ),
        );

        assert!(!result.allowed);
        assert_eq!(result.required_action, "ship-newer-web");
    }

    #[test]
    fn server_only_forward_only_requires_db_backup() {
        let result = run_preflight(
            PreflightMode::ServerOnly,
            &current(),
            &manifest(
                ReleaseUnits {
                    calm_server: Some(server(DbMigrationPolicy::ForwardOnly, 2)),
                    bundle: Some(BundleUnit {
                        binaries: bundle_binaries(),
                    }),
                    ..ReleaseUnits::default()
                },
                backend_files(),
            ),
        );

        assert!(result.allowed);
        assert!(result.requires_db_backup);
    }

    #[test]
    fn server_only_denies_destructive_db_policy() {
        let result = run_preflight(
            PreflightMode::ServerOnly,
            &current(),
            &manifest(
                ReleaseUnits {
                    calm_server: Some(server(DbMigrationPolicy::Destructive, 2)),
                    bundle: Some(BundleUnit {
                        binaries: bundle_binaries(),
                    }),
                    ..ReleaseUnits::default()
                },
                backend_files(),
            ),
        );

        assert!(!result.allowed);
        assert_eq!(result.required_action, "manual-migration-required");
    }

    #[test]
    fn bundle_checks_internal_web_server_compatibility() {
        let result = run_preflight(
            PreflightMode::Bundle,
            &current(),
            &manifest(
                ReleaseUnits {
                    web: Some(WebUnit {
                        version: "web".into(),
                        compatibility: compat(2, 2),
                    }),
                    calm_server: Some(server(DbMigrationPolicy::None, 3)),
                    bundle: Some(BundleUnit {
                        binaries: bundle_binaries(),
                    }),
                    ..ReleaseUnits::default()
                },
                bundle_files(),
            ),
        );

        assert!(!result.allowed);
        assert_eq!(result.required_action, "ship-newer-web-in-bundle");
    }

    #[test]
    fn app_only_requires_neige_app_unit() {
        let result = run_preflight(
            PreflightMode::AppOnly,
            &current(),
            &manifest(
                ReleaseUnits {
                    app: Some(AppUnit {
                        name: "other".into(),
                        version: "0.1.0".into(),
                    }),
                    ..ReleaseUnits::default()
                },
                vec![app_file()],
            ),
        );

        assert!(!result.allowed);
        assert_eq!(result.required_action, "provide-neige-app-unit");
    }

    #[test]
    fn verdict_noop_for_identical_manifest_and_state() {
        let manifest = v2_manifest(v2_units());
        let installed = installed_from(&manifest);

        assert_eq!(compute_verdict(&installed, &manifest), Verdict::Noop);
    }

    #[test]
    fn verdict_preserving_for_calm_server_change_requires_backup_by_policy() {
        let mut manifest = v2_manifest(v2_units());
        let installed = installed_from(&manifest);
        manifest.units.insert(
            UnitName::CalmServer,
            release_unit(
                "0.2.0",
                Some(&"d".repeat(64)),
                None,
                RestartPolicy::RestartViaAdminApi,
                Some(DbMigrationPolicy::ForwardOnly),
            ),
        );

        let verdict = compute_verdict(&installed, &manifest);

        assert_eq!(
            verdict,
            Verdict::Preserving {
                units_changed: vec![UnitName::CalmServer],
                deferred: vec![],
                refresh_frontend: false,
                requires_db_backup: true,
            }
        );
    }

    #[test]
    fn verdict_preserving_deferred_for_supervisor_change() {
        let mut manifest = v2_manifest(v2_units());
        let installed = installed_from(&manifest);
        manifest.units.insert(
            UnitName::CalmProcSupervisor,
            release_unit(
                "0.2.0",
                Some(&"e".repeat(64)),
                None,
                RestartPolicy::DeferUntilFullReboot,
                None,
            ),
        );

        let verdict = compute_verdict(&installed, &manifest);

        assert_eq!(
            verdict,
            Verdict::Preserving {
                units_changed: vec![UnitName::CalmProcSupervisor],
                deferred: vec![UnitName::CalmProcSupervisor],
                refresh_frontend: false,
                requires_db_backup: false,
            }
        );
    }

    #[test]
    fn verdict_preserving_refresh_frontend_for_web_change() {
        let mut manifest = v2_manifest(v2_units());
        let installed = installed_from(&manifest);
        manifest.units.insert(
            UnitName::Web,
            release_unit(
                "0.2.0",
                None,
                Some(&"f".repeat(64)),
                RestartPolicy::RefreshFrontend,
                None,
            ),
        );

        let verdict = compute_verdict(&installed, &manifest);

        assert_eq!(
            verdict,
            Verdict::Preserving {
                units_changed: vec![UnitName::Web],
                deferred: vec![],
                refresh_frontend: true,
                requires_db_backup: false,
            }
        );
    }

    #[test]
    fn verdict_breaking_when_product_major_changes() {
        let mut manifest = v2_manifest(v2_units());
        let installed = installed_from(&manifest);
        manifest.product_major = 1;

        let verdict = compute_verdict(&installed, &manifest);

        assert_eq!(
            verdict,
            Verdict::Breaking {
                reason: BreakingReason::ProductMajorChanged,
                units_changed: vec![],
            }
        );
    }

    #[test]
    fn verdict_breaking_when_web_compat_minimum_exceeds_installed_web() {
        let mut manifest = v2_manifest(v2_units());
        let installed = installed_from(&manifest);
        manifest.compatibility.min_web_compat_version = 3;

        let verdict = compute_verdict(&installed, &manifest);

        assert_eq!(
            verdict,
            Verdict::Breaking {
                reason: BreakingReason::WireIncompatibility,
                units_changed: vec![],
            }
        );
    }

    #[test]
    fn verdict_breaking_for_destructive_db_policy() {
        let mut manifest = v2_manifest(v2_units());
        let installed = installed_from(&manifest);
        manifest.units.insert(
            UnitName::CalmServer,
            release_unit(
                "0.2.0",
                Some(&"a".repeat(64)),
                None,
                RestartPolicy::RestartViaAdminApi,
                Some(DbMigrationPolicy::Destructive),
            ),
        );

        let verdict = compute_verdict(&installed, &manifest);

        assert_eq!(
            verdict,
            Verdict::Breaking {
                reason: BreakingReason::DestructiveDbMigration,
                units_changed: vec![UnitName::CalmServer],
            }
        );
    }

    #[test]
    fn verdict_breaking_when_installed_state_is_missing() {
        let manifest = v2_manifest(v2_units());

        let verdict = run_preflight_v2(None, &manifest);

        assert_eq!(
            verdict,
            Verdict::Breaking {
                reason: BreakingReason::NoInstalledState,
                units_changed: vec![
                    UnitName::CalmServer,
                    UnitName::CalmProcSupervisor,
                    UnitName::Web,
                ],
            }
        );
    }

    #[test]
    fn missing_installed_state_does_not_regress_v1_mode_based_preflight() {
        let result = run_preflight(
            PreflightMode::WebOnly,
            &current(),
            &manifest(
                ReleaseUnits {
                    web: Some(WebUnit {
                        version: "web".into(),
                        compatibility: compat(2, 2),
                    }),
                    ..ReleaseUnits::default()
                },
                vec![web_file()],
            ),
        );

        assert!(result.allowed);
        assert_eq!(result.mode, "web-only");
        assert!(result.verdict.is_none());
    }

    #[test]
    fn infer_mode_detects_common_shapes() {
        assert_eq!(
            infer_mode(&manifest(
                ReleaseUnits {
                    web: Some(WebUnit {
                        version: "web".into(),
                        compatibility: compat(2, 2),
                    }),
                    calm_server: Some(server(DbMigrationPolicy::None, 2)),
                    bundle: Some(BundleUnit {
                        binaries: bundle_binaries(),
                    }),
                    ..ReleaseUnits::default()
                },
                bundle_files(),
            ))
            .expect("bundle"),
            PreflightMode::Bundle
        );
        assert_eq!(
            infer_mode(&manifest(
                ReleaseUnits {
                    web: Some(WebUnit {
                        version: "web".into(),
                        compatibility: compat(2, 2),
                    }),
                    ..ReleaseUnits::default()
                },
                vec![web_file()],
            ))
            .expect("web-only"),
            PreflightMode::WebOnly
        );
        assert_eq!(
            infer_mode(&manifest(
                ReleaseUnits {
                    calm_server: Some(server(DbMigrationPolicy::None, 2)),
                    bundle: Some(BundleUnit {
                        binaries: bundle_binaries(),
                    }),
                    ..ReleaseUnits::default()
                },
                backend_files(),
            ))
            .expect("server-only"),
            PreflightMode::ServerOnly
        );
    }

    #[test]
    fn app_only_requires_app_file() {
        let result = run_preflight(
            PreflightMode::AppOnly,
            &current(),
            &manifest(
                ReleaseUnits {
                    app: Some(AppUnit {
                        name: "neige-app".into(),
                        version: "0.1.0".into(),
                    }),
                    ..ReleaseUnits::default()
                },
                Vec::new(),
            ),
        );

        assert!(!result.allowed);
        assert_eq!(result.required_action, "provide-app-file");
    }

    #[test]
    fn web_only_requires_valid_web_file_hash() {
        let mut bad = web_file();
        bad.sha256 = "not-a-sha".into();
        let result = run_preflight(
            PreflightMode::WebOnly,
            &current(),
            &manifest(
                ReleaseUnits {
                    web: Some(WebUnit {
                        version: "web".into(),
                        compatibility: compat(2, 2),
                    }),
                    ..ReleaseUnits::default()
                },
                vec![bad],
            ),
        );

        assert!(!result.allowed);
        assert_eq!(result.required_action, "provide-web-files");
    }

    #[test]
    fn server_only_additive_requires_db_backup() {
        let result = run_preflight(
            PreflightMode::ServerOnly,
            &current(),
            &manifest(
                ReleaseUnits {
                    calm_server: Some(server(DbMigrationPolicy::Additive, 2)),
                    bundle: Some(BundleUnit {
                        binaries: bundle_binaries(),
                    }),
                    ..ReleaseUnits::default()
                },
                backend_files(),
            ),
        );

        assert!(result.allowed);
        assert!(result.requires_db_backup);
    }

    #[test]
    fn server_only_requires_backend_sidecars() {
        let result = run_preflight(
            PreflightMode::ServerOnly,
            &current(),
            &manifest(
                ReleaseUnits {
                    calm_server: Some(server(DbMigrationPolicy::None, 2)),
                    ..ReleaseUnits::default()
                },
                vec![server_file()],
            ),
        );

        assert!(!result.allowed);
        assert_eq!(result.required_action, "provide-backend-bundle-unit");
    }

    #[test]
    fn bundle_requires_all_expected_binaries() {
        let result = run_preflight(
            PreflightMode::Bundle,
            &current(),
            &manifest(
                ReleaseUnits {
                    web: Some(WebUnit {
                        version: "web".into(),
                        compatibility: compat(2, 2),
                    }),
                    calm_server: Some(server(DbMigrationPolicy::None, 2)),
                    bundle: Some(BundleUnit {
                        binaries: vec![BinaryUnit {
                            name: "calm-server".into(),
                            path: "bin/calm-server".into(),
                        }],
                    }),
                    ..ReleaseUnits::default()
                },
                vec![server_file(), web_file()],
            ),
        );

        assert!(!result.allowed);
        assert_eq!(result.required_action, "provide-bundle-files");
    }

    #[test]
    fn bundle_denies_required_binary_mapped_outside_bin_name() {
        let mut binaries = bundle_binaries();
        binaries
            .iter_mut()
            .find(|binary| binary.name == "neige-codex-bridge")
            .expect("codex bridge binary")
            .path = "web/dist/index.html".into();

        let result = run_preflight(
            PreflightMode::Bundle,
            &current(),
            &manifest(
                ReleaseUnits {
                    web: Some(WebUnit {
                        version: "web".into(),
                        compatibility: compat(2, 2),
                    }),
                    calm_server: Some(server(DbMigrationPolicy::None, 2)),
                    bundle: Some(BundleUnit { binaries }),
                    ..ReleaseUnits::default()
                },
                bundle_files(),
            ),
        );

        assert!(!result.allowed);
        assert_eq!(result.required_action, "provide-bundle-files");
        assert!(result.reason.contains("must map to bin/neige-codex-bridge"));
    }

    #[test]
    fn bundle_denies_required_binary_with_wrong_file_unit() {
        let mut files = bundle_files();
        files
            .iter_mut()
            .find(|file| file.path == "bin/neige-codex-bridge")
            .expect("codex bridge file")
            .unit = FileUnit::Web;

        let result = run_preflight(
            PreflightMode::Bundle,
            &current(),
            &manifest(
                ReleaseUnits {
                    web: Some(WebUnit {
                        version: "web".into(),
                        compatibility: compat(2, 2),
                    }),
                    calm_server: Some(server(DbMigrationPolicy::None, 2)),
                    bundle: Some(BundleUnit {
                        binaries: bundle_binaries(),
                    }),
                    ..ReleaseUnits::default()
                },
                files,
            ),
        );

        assert!(!result.allowed);
        assert_eq!(result.required_action, "provide-bundle-files");
        assert!(result.reason.contains("unit Bundle"));
    }
}
