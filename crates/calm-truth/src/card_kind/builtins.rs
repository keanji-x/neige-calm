use serde::Deserialize;
use serde_json::Value;

use super::{
    CardCreateMode, CardKindError, CardKindHandler, CardKindMatcher, CardKindResult,
    CardPersistenceInvariants,
};
use crate::validation::{
    CLAUDE_PAYLOAD_SCHEMA_VERSION, CODEX_PAYLOAD_SCHEMA_VERSION, TERMINAL_PAYLOAD_SCHEMA_VERSION,
    WAVE_REPORT_PAYLOAD_SCHEMA_VERSION,
};

pub struct TerminalCardHandler;

impl CardKindHandler for TerminalCardHandler {
    fn kind_id(&self) -> &'static str {
        "terminal"
    }

    fn create_mode(&self) -> CardCreateMode {
        CardCreateMode::Atomic
    }

    fn schema_version(&self) -> Option<u32> {
        Some(TERMINAL_PAYLOAD_SCHEMA_VERSION)
    }

    fn validate_payload(&self, payload: &Value) -> CardKindResult<()> {
        #[derive(Deserialize)]
        #[allow(dead_code)]
        struct TerminalPayload {
            #[serde(default)]
            terminal_id: Option<String>,
        }

        if payload.is_null() {
            return Ok(());
        }
        check_schema_version(self.kind_id(), payload, TERMINAL_PAYLOAD_SCHEMA_VERSION)?;
        serde_json::from_value::<TerminalPayload>(payload.clone())
            .map(|_| ())
            .map_err(|e| bad(self.kind_id(), e))
    }
}

pub struct CodexCardHandler;

impl CardKindHandler for CodexCardHandler {
    fn kind_id(&self) -> &'static str {
        "codex"
    }

    fn create_mode(&self) -> CardCreateMode {
        CardCreateMode::Atomic
    }

    fn schema_version(&self) -> Option<u32> {
        Some(CODEX_PAYLOAD_SCHEMA_VERSION)
    }

    fn validate_payload(&self, payload: &Value) -> CardKindResult<()> {
        if payload.is_null() {
            return Ok(());
        }
        if !payload.is_object() {
            return Err(raw_bad("codex payload must be an object or null"));
        }
        check_schema_version(self.kind_id(), payload, CODEX_PAYLOAD_SCHEMA_VERSION)
    }
}

pub struct ClaudeCardHandler;

impl CardKindHandler for ClaudeCardHandler {
    fn kind_id(&self) -> &'static str {
        "claude"
    }

    fn create_mode(&self) -> CardCreateMode {
        CardCreateMode::Atomic
    }

    fn schema_version(&self) -> Option<u32> {
        Some(CLAUDE_PAYLOAD_SCHEMA_VERSION)
    }

    fn validate_payload(&self, payload: &Value) -> CardKindResult<()> {
        if payload.is_null() {
            return Ok(());
        }
        if !payload.is_object() {
            return Err(raw_bad("claude payload must be an object or null"));
        }
        check_schema_version(self.kind_id(), payload, CLAUDE_PAYLOAD_SCHEMA_VERSION)
    }
}

pub struct WaveReportCardHandler;

impl CardKindHandler for WaveReportCardHandler {
    fn kind_id(&self) -> &'static str {
        "wave-report"
    }

    fn create_mode(&self) -> CardCreateMode {
        CardCreateMode::KernelMintedOnly
    }

    fn schema_version(&self) -> Option<u32> {
        Some(WAVE_REPORT_PAYLOAD_SCHEMA_VERSION)
    }

    fn persistence_invariants(&self) -> CardPersistenceInvariants {
        CardPersistenceInvariants {
            deletable_after_create: false,
            unique_per_wave: true,
        }
    }

    fn validate_payload(&self, payload: &Value) -> CardKindResult<()> {
        #[derive(Deserialize)]
        #[allow(dead_code)]
        #[serde(rename_all = "camelCase")]
        struct WaveReportShape {
            #[serde(default)]
            schema_version: Option<u32>,
            summary: String,
            body: String,
        }

        check_schema_version(self.kind_id(), payload, WAVE_REPORT_PAYLOAD_SCHEMA_VERSION)?;
        serde_json::from_value::<WaveReportShape>(payload.clone())
            .map(|_| ())
            .map_err(|e| bad(self.kind_id(), e))
    }
}

pub struct SpecCardHandler;

impl CardKindHandler for SpecCardHandler {
    fn kind_id(&self) -> &'static str {
        "spec"
    }

    fn create_mode(&self) -> CardCreateMode {
        CardCreateMode::KernelMintedOnly
    }

    fn persistence_invariants(&self) -> CardPersistenceInvariants {
        CardPersistenceInvariants {
            deletable_after_create: false,
            unique_per_wave: true,
        }
    }

    fn validate_payload(&self, _payload: &Value) -> CardKindResult<()> {
        Ok(())
    }
}

pub struct PluginUiCardHandler;

impl CardKindHandler for PluginUiCardHandler {
    fn kind_id(&self) -> &'static str {
        "ui"
    }

    fn matcher(&self) -> CardKindMatcher {
        CardKindMatcher::Prefix("ui://")
    }

    fn validate_payload(&self, _payload: &Value) -> CardKindResult<()> {
        Ok(())
    }
}

fn bad(kind: &str, msg: impl ToString) -> CardKindError {
    CardKindError::BadPayload {
        kind: kind.into(),
        message: msg.to_string(),
    }
}

fn raw_bad(msg: impl ToString) -> CardKindError {
    CardKindError::BadRequest(msg.to_string())
}

fn check_schema_version(kind: &str, payload: &Value, expected: u32) -> CardKindResult<()> {
    if !payload.is_object() {
        return Ok(());
    }
    let Some(raw) = payload.get("schemaVersion") else {
        return Ok(());
    };
    let Some(version) = raw.as_u64() else {
        return Err(raw_bad(format!(
            "invalid schemaVersion for kind `{kind}`: expected u32, got {raw}"
        )));
    };
    if version as u32 == expected {
        Ok(())
    } else {
        Err(raw_bad(format!(
            "unsupported schemaVersion {version} for kind `{kind}`; this kernel supports {expected}"
        )))
    }
}
