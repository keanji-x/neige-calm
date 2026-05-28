use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ReleaseManifest {
    pub schema_version: u32,
    pub release_id: String,
    pub units: ReleaseUnits,
    #[serde(default)]
    pub files: Vec<FileManifest>,
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
    pub compatibility: Compatibility,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct CalmServerUnit {
    pub version: String,
    pub compatibility: Compatibility,
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
pub(crate) struct Compatibility {
    pub api_version: String,
    pub sync_event_version: u32,
    pub mcp_protocol_version: String,
    pub web_compat_version: u32,
    pub min_web_compat_version: u32,
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
}

impl CurrentVersion {
    pub(crate) fn conservative_web_compat_version(&self) -> u32 {
        self.web_compat_version
            .unwrap_or(self.min_web_compat_version)
    }
}
