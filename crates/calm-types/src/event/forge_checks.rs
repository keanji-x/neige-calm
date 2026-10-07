use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Evidence captured by the checks read, never reconstructed from a later PR head.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ForgeChecksSnapshot {
    pub head_sha: String,
    pub mergeable: String,
    /// Absent only on historical snapshots, never inferred from their conclusion.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub all_checks_completed: Option<bool>,
}

/// One check the checks read classified as failed, and where to read it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub struct ForgeFailedCheck {
    pub name: String,
    #[serde(flatten)]
    pub locator: ForgeCheckLocator,
    /// Absent only on historical failures recorded before diagnostic capture.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub diagnostics: Option<ForgeCheckDiagnostics>,
}

/// Structured evidence from the exact Actions job, or an explicit collection failure.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ForgeCheckDiagnostics {
    Available {
        failed_tests: Vec<String>,
        failed_steps: Vec<String>,
        error_summary: String,
        log_url: String,
        truncated: bool,
    },
    Unavailable {
        reason: String,
    },
}

impl<'de> Deserialize<'de> for ForgeCheckDiagnostics {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(
            remote = "ForgeCheckDiagnostics",
            tag = "status",
            rename_all = "snake_case"
        )]
        enum Wire {
            Available {
                failed_tests: Vec<String>,
                failed_steps: Vec<String>,
                error_summary: String,
                log_url: String,
                truncated: bool,
            },
            Unavailable {
                reason: String,
            },
        }
        let value = Wire::deserialize(deserializer)?;
        let valid = match &value {
            Self::Available {
                failed_tests,
                failed_steps,
                error_summary,
                log_url,
                ..
            } => {
                failed_tests
                    .iter()
                    .chain(failed_steps)
                    .any(|s| !s.trim().is_empty())
                    && !error_summary.trim().is_empty()
                    && !log_url.trim().is_empty()
            }
            Self::Unavailable { reason } => !reason.trim().is_empty(),
        };
        if !valid {
            return Err(serde::de::Error::custom(
                "checks diagnostics require failure evidence or an unavailable reason",
            ));
        }
        Ok(value)
    }
}

/// A failed check's details URL, or the forge's own id for it when it has none.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(untagged)]
#[ts(export, export_to = "fe/core/api/generated/wire.ts")]
pub enum ForgeCheckLocator {
    Url { url: String },
    Id { id: String },
}
