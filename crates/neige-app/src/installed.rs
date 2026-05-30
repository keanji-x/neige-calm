use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::manifest::{Compatibility, ReleaseManifestV2, UnitName};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstalledState {
    pub schema_version: u32,
    pub release_id: String,
    pub product_major: u32,
    pub compatibility: Compatibility,
    pub units: BTreeMap<UnitName, InstalledUnit>,
    pub installed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct InstalledUnit {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_sha256: Option<String>,
}

impl InstalledState {
    pub(crate) fn from_manifest(manifest: &ReleaseManifestV2) -> Self {
        Self {
            schema_version: 1,
            release_id: manifest.release_id.clone(),
            product_major: manifest.product_major,
            compatibility: manifest.compatibility.clone(),
            units: manifest
                .units
                .iter()
                .map(|(name, unit)| {
                    (
                        *name,
                        InstalledUnit {
                            version: unit.version.clone(),
                            binary_sha256: unit.binary_sha256.clone(),
                            tree_sha256: unit.tree_sha256.clone(),
                        },
                    )
                })
                .collect(),
            installed_at: chrono::Utc::now().to_rfc3339(),
        }
    }
}

pub(crate) fn installed_state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("state").join("installed.json")
}

pub(crate) fn read_installed_state(data_dir: &Path) -> anyhow::Result<Option<InstalledState>> {
    let path = installed_state_path(data_dir);
    match fs::read(&path) {
        Ok(bytes) => {
            let state = serde_json::from_slice(&bytes)
                .with_context(|| format!("parse installed state {}", path.display()))?;
            Ok(Some(state))
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err).with_context(|| format!("read installed state {}", path.display())),
    }
}

pub(crate) fn write_installed_state(data_dir: &Path, state: &InstalledState) -> anyhow::Result<()> {
    let path = installed_state_path(data_dir);
    write_json_atomic(&path, state)
}

pub(crate) fn write_json_atomic<T>(path: &Path, value: &T) -> anyhow::Result<()>
where
    T: Serialize,
{
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let temp = path.with_extension(format!("json.tmp.{}", std::process::id()));
    if temp.exists() {
        fs::remove_file(&temp).with_context(|| format!("remove {}", temp.display()))?;
    }

    let bytes = serde_json::to_vec_pretty(value)?;
    {
        let mut file =
            fs::File::create(&temp).with_context(|| format!("create {}", temp.display()))?;
        use std::io::Write as _;
        file.write_all(&bytes)
            .with_context(|| format!("write {}", temp.display()))?;
        file.sync_all()
            .with_context(|| format!("fsync {}", temp.display()))?;
    }

    if let Err(err) = fs::rename(&temp, path) {
        let _ = fs::remove_file(&temp);
        return Err(err)
            .with_context(|| format!("rename {} to {}", temp.display(), path.display()));
    }
    if let Some(parent) = path.parent()
        && let Ok(dir) = fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::{Compatibility, DbMigrationPolicy, ReleaseUnit, RestartPolicy};

    fn state() -> InstalledState {
        let mut units = BTreeMap::new();
        units.insert(
            UnitName::CalmServer,
            InstalledUnit {
                version: "0.1.0".into(),
                binary_sha256: Some("a".repeat(64)),
                tree_sha256: None,
            },
        );
        InstalledState {
            schema_version: 1,
            release_id: "rel".into(),
            product_major: 0,
            compatibility: Compatibility {
                terminal_frame_version: 4,
                terminal_protocol_version: 4,
                api_version: "1".into(),
                sync_event_version: 1,
                mcp_protocol_version: "2024-11-05".into(),
                plugin_mcp_protocol_version: "2025-11-25".into(),
                web_compat_version: 2,
                min_web_compat_version: 2,
                supervisor_control_version: 1,
            },
            units,
            installed_at: "2026-05-30T00:00:00Z".into(),
        }
    }

    #[test]
    fn installed_state_round_trips_through_json() {
        let state = state();
        let bytes = serde_json::to_vec(&state).expect("serialize");
        let decoded: InstalledState = serde_json::from_slice(&bytes).expect("deserialize");

        assert_eq!(decoded.release_id, state.release_id);
        assert_eq!(
            decoded.units.get(&UnitName::CalmServer),
            state.units.get(&UnitName::CalmServer)
        );
    }

    #[test]
    fn installed_state_from_manifest_copies_units() {
        let mut units = BTreeMap::new();
        units.insert(
            UnitName::CalmServer,
            ReleaseUnit {
                version: "0.2.0".into(),
                binary_sha256: Some("b".repeat(64)),
                tree_sha256: None,
                restart_policy: RestartPolicy::RestartViaAdminApi,
                db_migration_policy: Some(DbMigrationPolicy::ForwardOnly),
            },
        );
        let manifest = ReleaseManifestV2 {
            schema_version: 2,
            release_id: "target".into(),
            product_major: 0,
            compatibility: state().compatibility,
            units,
            files: Vec::new(),
        };

        let installed = InstalledState::from_manifest(&manifest);

        assert_eq!(installed.release_id, "target");
        assert_eq!(
            installed.units[&UnitName::CalmServer].binary_sha256,
            Some("b".repeat(64))
        );
    }

    #[test]
    fn atomic_write_cleans_temp_file_when_rename_fails() {
        let root = test_temp_dir("installed-rename-fails");
        let target = root.join("state").join("installed.json");
        fs::create_dir_all(&target).expect("create blocking directory");

        let err = write_json_atomic(&target, &state()).expect_err("rename over dir must fail");

        assert!(err.to_string().contains("rename"));
        assert!(target.is_dir());
        let tmp = target.with_extension(format!("json.tmp.{}", std::process::id()));
        assert!(
            !tmp.exists(),
            "temporary file must be cleaned after failure"
        );
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
