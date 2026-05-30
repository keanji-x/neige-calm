use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseManifestV1 {
    pub schema_version: u32,
    pub release_id: String,
    pub units: ReleaseUnits,
    #[serde(default)]
    pub files: Vec<FileManifest>,
}

pub(crate) type ReleaseManifest = ReleaseManifestV1;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseManifestV2 {
    pub schema_version: u32,
    pub release_id: String,
    pub product_major: u32,
    pub compatibility: Compatibility,
    pub units: BTreeMap<UnitName, ReleaseUnit>,
    #[serde(default)]
    pub files: Vec<FileManifest>,
}

#[derive(Debug, Clone)]
pub(crate) enum VersionedReleaseManifest {
    V1(ReleaseManifestV1),
    V2(ReleaseManifestV2),
}

impl VersionedReleaseManifest {
    pub(crate) fn release_id(&self) -> &str {
        match self {
            Self::V1(manifest) => &manifest.release_id,
            Self::V2(manifest) => &manifest.release_id,
        }
    }

    pub(crate) fn files(&self) -> &[FileManifest] {
        match self {
            Self::V1(manifest) => &manifest.files,
            Self::V2(manifest) => &manifest.files,
        }
    }
}

pub(crate) fn parse_versioned_manifest(bytes: &[u8]) -> anyhow::Result<VersionedReleaseManifest> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Header {
        schema_version: u32,
    }

    let header: Header = serde_json::from_slice(bytes)?;
    match header.schema_version {
        1 => Ok(VersionedReleaseManifest::V1(serde_json::from_slice(bytes)?)),
        2 => Ok(VersionedReleaseManifest::V2(serde_json::from_slice(bytes)?)),
        other => anyhow::bail!("unsupported manifest schemaVersion {other}"),
    }
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseUnits {
    pub app: Option<AppUnit>,
    pub web: Option<WebUnit>,
    pub calm_server: Option<CalmServerUnit>,
    pub bundle: Option<BundleUnit>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct AppUnit {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct WebUnit {
    pub version: String,
    pub compatibility: CompatibilityV1,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CalmServerUnit {
    pub version: String,
    pub compatibility: CompatibilityV1,
    pub db_migration_policy: DbMigrationPolicy,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BundleUnit {
    pub binaries: Vec<BinaryUnit>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct BinaryUnit {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CompatibilityV1 {
    pub api_version: String,
    pub sync_event_version: u32,
    pub mcp_protocol_version: String,
    pub web_compat_version: u32,
    pub min_web_compat_version: u32,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Compatibility {
    pub terminal_frame_version: u16,
    pub terminal_protocol_version: u16,
    pub api_version: String,
    pub sync_event_version: u32,
    pub mcp_protocol_version: String,
    pub plugin_mcp_protocol_version: String,
    pub web_compat_version: u32,
    pub min_web_compat_version: u32,
    pub supervisor_control_version: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum UnitName {
    NeigeApp,
    CalmServer,
    CalmProcSupervisor,
    Web,
    NeigeCodexBridge,
    NeigeMcpStdioShim,
    NeigeCli,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseUnit {
    pub version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tree_sha256: Option<String>,
    pub restart_policy: RestartPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_migration_policy: Option<DbMigrationPolicy>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum RestartPolicy {
    RestartViaAdminApi,
    DeferUntilFullReboot,
    RefreshFrontend,
    NextSpawn,
    ExecSelfForBreakingOnly,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum DbMigrationPolicy {
    None,
    Additive,
    ForwardOnly,
    Destructive,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct FileManifest {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub unit: FileUnit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) enum FileUnit {
    App,
    Web,
    CalmServer,
    Bundle,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CurrentVersion {
    pub api_version: String,
    pub sync_event_version: u32,
    pub mcp_protocol_version: String,
    pub min_web_compat_version: u32,
    #[serde(default)]
    pub web_compat_version: Option<u32>,
    #[serde(default)]
    pub plugin_mcp_protocol_version: Option<String>,
    #[serde(default)]
    pub supervisor_control_version: Option<u32>,
}

impl CurrentVersion {
    pub(crate) fn conservative_web_compat_version(&self) -> u32 {
        self.web_compat_version
            .unwrap_or(self.min_web_compat_version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_manifest_still_parses_through_version_router() {
        let bytes = br#"{
            "schemaVersion": 1,
            "releaseId": "legacy",
            "units": { "app": { "name": "neige-app", "version": "0.1.0" } },
            "files": []
        }"#;

        let manifest = parse_versioned_manifest(bytes).expect("parse v1");
        match manifest {
            VersionedReleaseManifest::V1(v1) => assert_eq!(v1.release_id, "legacy"),
            VersionedReleaseManifest::V2(_) => panic!("expected v1 manifest"),
        }
    }

    #[test]
    fn v2_manifest_parses_with_per_crate_units() {
        let bytes = br#"{
            "schemaVersion": 2,
            "releaseId": "v2",
            "productMajor": 0,
            "compatibility": {
                "terminalFrameVersion": 4,
                "terminalProtocolVersion": 4,
                "apiVersion": "1",
                "syncEventVersion": 1,
                "mcpProtocolVersion": "2024-11-05",
                "pluginMcpProtocolVersion": "2025-11-25",
                "webCompatVersion": 2,
                "minWebCompatVersion": 2,
                "supervisorControlVersion": 1
            },
            "units": {
                "calmServer": {
                    "version": "0.1.0",
                    "binarySha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "restartPolicy": "restartViaAdminApi",
                    "dbMigrationPolicy": "none"
                }
            },
            "files": []
        }"#;

        let manifest = parse_versioned_manifest(bytes).expect("parse v2");
        match manifest {
            VersionedReleaseManifest::V1(_) => panic!("expected v2 manifest"),
            VersionedReleaseManifest::V2(v2) => {
                assert_eq!(v2.product_major, 0);
                assert!(v2.units.contains_key(&UnitName::CalmServer));
            }
        }
    }
}
