//! `forge-action` operation adapter.
//!
//! Forge actions are crash-safety critical: the irreversible action is
//! held behind a stdin handshake until the operation row is durably parked.
//! The post-park observer owns the child + stdin handle and releases the
//! token as its first awaited step.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::io::AsyncWriteExt;

use crate::db::RouteRepo;
use crate::db::sqlite::{append_decision_event_in_tx, begin_immediate_tx};
use crate::error::{CalmError, Result};
use crate::event::{
    BroadcastEnvelope, Event, EventBus, EventScope, FieldSource, ForgeEventSpec, ForgeMergeSubject,
    SYNC_EVENT_VERSION,
};
use crate::ids::{ActorId, CardId, CoveId, WaveId};
use crate::proc_identity::{
    read_boot_id, read_proc_start_time, signal_process_group, verify_owned_pid,
};
use calm_truth::decision_gate::PermissiveGate;

use super::{
    AppServerInteractOutcome, CompensationStateVersioned, CompensationStep, Operation,
    OperationCompletionBus, ParkedCompletion, ParkedOutcome, ParkedRecovery, PhaseTag,
    ProviderAdapter, RecoveryMode, SpawnArtifacts, SpawnCtx, SpawnOutcome, Tx, TxOutput,
    complete_parked_tx,
};

pub const FORGE_ACTION_KIND: &str = "forge-action";

/// The forge event kinds the adapter can construct via Event::from_kind_and_payload
/// and persist. validate_payload rejects any other event_kind BEFORE the irreversible
/// action can run, so a typo'd/unsupported kind can never execute the side effect and
/// then fail to record its authoritative event. Slice ③ appends its forge.* kinds here.
pub const SUPPORTED_FORGE_EVENT_KINDS: &[&str] = &[
    "forge.pr.merged",
    "forge.scan.completed",
    "forge.pr.opened",
    "forge.pr.diff.read",
    "forge.issue.read",
    "forge.pr.checks",
    "forge.issue.closed",
    "worktree.provisioned",
    "worktree.committed",
    "worktree.removed",
];

const RELEASE_TIMEOUT: Duration = Duration::from_secs(60);
const REATTACH_POLL: Duration = Duration::from_secs(2);
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
static NEXT_FORGE_ARTIFACT_TMP: AtomicU64 = AtomicU64::new(1);
const FORGE_BASE_ENV_KEYS: &[&str] = &["PATH", "HOME", "LANG", "LC_ALL", "TERM"];
const FORGE_PASSTHROUGH_ENV_KEYS: &[&str] = &[
    "GH_TOKEN",
    "GITHUB_TOKEN",
    "GH_HOST",
    "GH_ENTERPRISE_TOKEN",
    "GITHUB_ENTERPRISE_TOKEN",
    "SSH_AUTH_SOCK",
    "GIT_SSH_COMMAND",
    "NO_PROXY",
    "no_proxy",
];

const FORGE_ACTION_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::SpawnStarted,
    PhaseTag::Parked,
    PhaseTag::Succeeded,
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForgeActionPayload {
    pub wave_id: String,
    pub card_id: String,
    #[serde(default)]
    pub subject: Option<ForgeMergeSubject>,
    pub argv: Vec<String>,
    pub idem_key: String,
    #[serde(default)]
    pub event_spec: Option<ForgeEventSpec>,
    #[serde(default)]
    pub context: Map<String, Value>,
    #[serde(default)]
    pub probe: Option<ProbeSpec>,
    pub cwd_lease: PathBuf,
    pub result_path: PathBuf,
    pub deadline_ms: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProbeSpec {
    pub probe_argv: Vec<String>,
    #[serde(default)]
    pub output_probe_argv: Option<Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct FrozenForge {
    pub wave_id: String,
    pub cove_id: String,
    pub card_id: String,
    #[serde(default)]
    pub subject: Option<ForgeMergeSubject>,
    pub argv: Vec<String>,
    pub idem_key: String,
    #[serde(default)]
    pub event_spec: Option<ForgeEventSpec>,
    #[serde(default)]
    pub context: Map<String, Value>,
    #[serde(default)]
    pub probe: Option<ProbeSpec>,
    pub cwd_lease: PathBuf,
    pub result_path: PathBuf,
    pub deadline_ms: i64,
}

impl FrozenForge {
    pub(crate) fn from_output(output: &TxOutput) -> Result<Self> {
        serde_json::from_value(output.data.clone()).map_err(|e| {
            CalmError::Internal(format!("forge-action tx_output.data unparseable: {e}"))
        })
    }

    fn event_scope_for(&self, event_kind: &str) -> EventScope {
        if event_kind.starts_with("worktree.") {
            EventScope::Card {
                card: CardId::from(self.card_id.clone()),
                wave: WaveId::from(self.wave_id.clone()),
                cove: CoveId::from(self.cove_id.clone()),
            }
        } else {
            EventScope::Wave {
                wave: WaveId::from(self.wave_id.clone()),
                cove: CoveId::from(self.cove_id.clone()),
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[derive(Clone, Debug)]
pub struct ForgeActionAdapter;

impl ForgeActionAdapter {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ForgeActionAdapter {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Debug, Deserialize)]
struct ForgeActionResultFile {
    exit_code: i32,
    stdout: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProbeVerdict {
    Landed,
    NotLanded,
    Unknown,
}

#[derive(Debug, PartialEq, Eq)]
enum ForgeEventBuildError {
    ActionFailed { reason: String },
    ExtractionFailed { reason: String },
}

#[derive(Clone, Copy)]
struct ForgeCompletionRefs<'a> {
    pool: &'a sqlx::SqlitePool,
    completion: &'a OperationCompletionBus,
    events: &'a EventBus,
    repo: &'a dyn RouteRepo,
}

/// POSIX single-quote escaping: `'` -> `'\''`.
fn sh_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn render_forge_wrapper(argv: &[String]) -> Result<String> {
    if argv.is_empty() {
        return Err(CalmError::BadRequest(
            "forge-action argv must not be empty".into(),
        ));
    }
    let rendered_argv = argv
        .iter()
        .map(|arg| sh_single_quote(arg))
        .collect::<Vec<_>>()
        .join(" ");
    let mut script = String::new();
    script.push_str("#!/bin/sh\n");
    script.push_str("# generated by neige-calm forge-action; do not edit\n");
    script.push_str("read -r _go || exit 75\n");
    script.push_str("neige_forge_result_path=$NEIGE_FORGE_RESULT_PATH\n");
    script.push_str("unset NEIGE_FORGE_RESULT_PATH\n");
    script.push_str("neige_forge_code_path=\"${neige_forge_result_path}.code\"\n");
    script.push_str("neige_forge_stdout_path=\"${neige_forge_result_path}.stdout\"\n");
    script.push_str("neige_forge_code_tmp_path=\"${neige_forge_code_path}.tmp.$$\"\n");
    script.push_str("neige_forge_stdout_tmp_path=\"${neige_forge_stdout_path}.tmp.$$\"\n");
    script.push_str("neige_forge_finish() {\n");
    script.push_str("  neige_forge_rc=\"$1\"\n");
    script.push_str("  mv -f -- \"$neige_forge_stdout_tmp_path\" \"$neige_forge_stdout_path\"\n");
    script.push_str("  neige_forge_stdout_mv_rc=$?\n");
    script.push_str(
        "  if [ \"$neige_forge_stdout_mv_rc\" -ne 0 ]; then \
         exit \"$neige_forge_stdout_mv_rc\"; fi\n",
    );
    script.push_str(
        "  printf '%s' \"$neige_forge_rc\" > \"$neige_forge_code_tmp_path\" && \
         mv -f -- \"$neige_forge_code_tmp_path\" \"$neige_forge_code_path\"\n",
    );
    script.push_str("  neige_forge_code_write_rc=$?\n");
    script.push_str(
        "  if [ \"$neige_forge_code_write_rc\" -ne 0 ]; then \
         exit \"$neige_forge_code_write_rc\"; fi\n",
    );
    script.push_str("  exit \"$neige_forge_rc\"\n");
    script.push_str("}\n");
    script.push_str("(\n");
    script.push_str("  exec ");
    script.push_str(&rendered_argv);
    script.push('\n');
    script.push_str(") > \"$neige_forge_stdout_tmp_path\"\n");
    script.push_str("neige_forge_rc=$?\n");
    script.push_str("neige_forge_finish \"$neige_forge_rc\"\n");
    Ok(script)
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut path = path.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn result_code_path(result_path: &Path) -> PathBuf {
    path_with_suffix(result_path, ".code")
}

fn result_stdout_path(result_path: &Path) -> PathBuf {
    path_with_suffix(result_path, ".stdout")
}

fn forge_artifact_tmp_path(result_path: &Path) -> PathBuf {
    let attempt = NEXT_FORGE_ARTIFACT_TMP.fetch_add(1, Ordering::Relaxed);
    path_with_suffix(
        result_path,
        &format!(".tmp.{}.{}", std::process::id(), attempt),
    )
}

fn result_tmp_prefixes(result_path: &Path) -> Vec<PathBuf> {
    vec![
        path_with_suffix(result_path, ".tmp"),
        path_with_suffix(result_path, ".code.tmp."),
        path_with_suffix(result_path, ".stdout."),
    ]
}

async fn remove_stale_result_files(result_path: &Path) -> Result<()> {
    for stale_path in [
        result_path.to_path_buf(),
        result_code_path(result_path),
        result_stdout_path(result_path),
    ] {
        match tokio::fs::remove_file(stale_path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
    }
    for prefix in result_tmp_prefixes(result_path) {
        if let Some(parent) = prefix.parent() {
            let Some(name_prefix) = prefix.file_name().and_then(|s| s.to_str()) else {
                continue;
            };
            let mut entries = match tokio::fs::read_dir(parent).await {
                Ok(entries) => entries,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            while let Some(entry) = entries.next_entry().await? {
                let name = entry.file_name();
                if name.to_string_lossy().starts_with(name_prefix) {
                    match tokio::fs::remove_file(entry.path()).await {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                        Err(e) => return Err(e.into()),
                    }
                }
            }
        }
    }
    Ok(())
}

fn kill_artifacts_group(artifacts: &SpawnArtifacts) {
    if verify_owned_pid(artifacts.pid, artifacts.start_time, &artifacts.boot_id) {
        signal_process_group(artifacts.pgid, libc::SIGKILL);
    }
}

fn forge_passthrough_env_from<F>(mut lookup: F) -> Vec<(&'static str, std::ffi::OsString)>
where
    F: FnMut(&str) -> Option<std::ffi::OsString>,
{
    FORGE_PASSTHROUGH_ENV_KEYS
        .iter()
        .filter_map(|&key| lookup(key).map(|value| (key, value)))
        .collect()
}

#[cfg(any(test, feature = "fixtures"))]
pub fn forge_passthrough_env_for_test<F>(lookup: F) -> Vec<(&'static str, String)>
where
    F: FnMut(&str) -> Option<std::ffi::OsString>,
{
    forge_passthrough_env_from(lookup)
        .into_iter()
        .map(|(key, value)| (key, value.to_string_lossy().into_owned()))
        .collect()
}

/// Build the forge subprocess environment: env_clear + a tight allowlist
/// (base PATH/HOME/..., settings-based proxy, runtime auth/proxy passthrough).
/// Applied identically to the action and to recovery probes — both run
/// plugin-supplied argv and both need gh auth + proxy, neither may inherit
/// daemon secrets.
async fn apply_forge_subprocess_env(cmd: &mut tokio::process::Command, repo: &dyn RouteRepo) {
    cmd.env_clear();
    for key in FORGE_BASE_ENV_KEYS {
        if let Some(v) = std::env::var_os(key) {
            cmd.env(key, v);
        }
    }
    if let Value::Object(env) = super::terminal_adapter::terminal_worker_env(repo)
        .await
        .unwrap_or(Value::Null)
    {
        for (k, v) in env {
            if let Value::String(v) = v {
                cmd.env(k, v);
            }
        }
    }
    for (key, value) in forge_passthrough_env_from(|key| std::env::var_os(key)) {
        cmd.env(key, value);
    }
}

fn forge_spec_needs_json(spec: Option<&ForgeEventSpec>) -> bool {
    spec.map(|spec| {
        spec.fields
            .values()
            .any(|source| matches!(source, FieldSource::JsonField { .. }))
    })
    .unwrap_or(false)
}

fn required_output_fields(event_kind: &str) -> &'static [(&'static str, bool)] {
    match event_kind {
        "forge.pr.merged" => &[("head_sha", true), ("merge_sha", true)],
        _ => &[],
    }
}

#[derive(Clone, Copy)]
enum ForgeFieldType {
    Str,
    U64,
    VecU64,
}

impl ForgeFieldType {
    fn matches_context_value(self, value: &Value) -> bool {
        match self {
            Self::Str => value.is_string(),
            Self::U64 => value.is_u64(),
            Self::VecU64 => value
                .as_array()
                .is_some_and(|items| items.iter().all(Value::is_u64)),
        }
    }

    fn json_type_name(self) -> &'static str {
        match self {
            Self::Str => "string",
            Self::U64 => "u64",
            Self::VecU64 => "u64 array",
        }
    }
}

/// Required non-kernel payload fields for the new slice ③ event kinds and
/// their JSON type. `forge.pr.merged` stays on `required_output_fields`.
fn new_kind_required_fields(event_kind: &str) -> &'static [(&'static str, ForgeFieldType)] {
    use ForgeFieldType::*;
    match event_kind {
        "forge.scan.completed" => &[("overlapping_prs", VecU64)],
        "forge.pr.opened" => &[("pr_number", U64), ("head_sha", Str)],
        "forge.pr.diff.read" => &[
            ("pr_number", U64),
            ("base_sha", Str),
            ("head_sha", Str),
            ("artifact_path", Str),
        ],
        "forge.issue.read" => &[("issue_number", U64), ("artifact_path", Str)],
        "forge.pr.checks" => &[("pr_number", U64), ("conclusion", Str)],
        "forge.issue.closed" => &[("issue_number", U64)],
        "worktree.provisioned" | "worktree.removed" => &[("path", Str)],
        "worktree.committed" => &[("commit_sha", Str), ("branch", Str)],
        _ => &[],
    }
}

/// Kernel-authoritative payload fields the kernel injects last and that
/// plugin `context`/`event_spec.fields` may not set: `wave_id` for every
/// kind; `subject` for `forge.pr.merged`; `card_id` for `worktree.*`.
fn kernel_injected_fields(event_kind: &str) -> &'static [&'static str] {
    match event_kind {
        "forge.pr.merged" => &["wave_id", "subject"],
        "forge.pr.diff.read" | "forge.issue.read" => &["wave_id", "artifact_path"],
        "worktree.provisioned" | "worktree.committed" | "worktree.removed" => {
            &["wave_id", "card_id"]
        }
        _ => &["wave_id"],
    }
}

fn field_source_type_name(source: &FieldSource) -> &'static str {
    match source {
        FieldSource::ExitCode => "exit_code",
        FieldSource::JsonField { .. } => "json_field",
    }
}

fn parse_json_stdout_if_needed(
    spec: Option<&ForgeEventSpec>,
    stdout: &str,
) -> Result<Option<Value>> {
    if !forge_spec_needs_json(spec) {
        return Ok(None);
    }
    serde_json::from_str::<Value>(stdout)
        .map(Some)
        .map_err(|e| {
            CalmError::Internal(format!(
                "gate-infra: forge action JSON stdout unparseable: {e}"
            ))
        })
}

fn validate_payload(payload: &ForgeActionPayload) -> Result<()> {
    if payload.wave_id.trim().is_empty() {
        return Err(CalmError::BadRequest(
            "forge-action wave_id must not be empty".into(),
        ));
    }
    if payload.card_id.trim().is_empty() {
        return Err(CalmError::BadRequest(
            "forge-action card_id must not be empty".into(),
        ));
    }
    if payload.argv.is_empty() {
        return Err(CalmError::BadRequest(
            "forge-action argv must not be empty".into(),
        ));
    }
    if payload.idem_key.trim().is_empty() {
        return Err(CalmError::BadRequest(
            "forge-action idem_key must not be empty".into(),
        ));
    }
    match (
        payload
            .event_spec
            .as_ref()
            .map(|spec| spec.event_kind.as_str()),
        payload.subject.as_ref(),
    ) {
        (Some("forge.pr.merged"), None) => {
            return Err(CalmError::BadRequest(
                "forge.pr.merged requires subject".into(),
            ));
        }
        (Some("forge.pr.merged"), Some(_)) => {}
        (_, Some(_)) => {
            return Err(CalmError::BadRequest(
                "subject is only valid for forge.pr.merged".into(),
            ));
        }
        (_, None) => {}
    }
    if let Some(spec) = payload.event_spec.as_ref() {
        if spec.event_kind.trim().is_empty() {
            return Err(CalmError::BadRequest(
                "forge-action event_spec.event_kind must not be empty".into(),
            ));
        }
        if !SUPPORTED_FORGE_EVENT_KINDS.contains(&spec.event_kind.as_str()) {
            return Err(CalmError::BadRequest(format!(
                "forge-action event_kind `{}` is not a supported forge event kind",
                spec.event_kind
            )));
        }
        for reserved in kernel_injected_fields(&spec.event_kind) {
            if payload.context.contains_key(*reserved) || spec.fields.contains_key(*reserved) {
                return Err(CalmError::BadRequest(format!(
                    "forge event context/output may not set reserved key `{reserved}`"
                )));
            }
        }
        for (field, field_type) in new_kind_required_fields(&spec.event_kind) {
            if kernel_injected_fields(&spec.event_kind).contains(field) {
                continue;
            }
            if let Some(source) = spec.fields.get(*field) {
                if !matches!(source, FieldSource::JsonField { .. }) {
                    return Err(CalmError::BadRequest(format!(
                        "forge-action `{}` field `{field}` must be extracted via a JSON field source, not exit_code",
                        spec.event_kind
                    )));
                }
            } else if let Some(value) = payload.context.get(*field) {
                if !field_type.matches_context_value(value) {
                    return Err(CalmError::BadRequest(format!(
                        "forge-action `{}` context field `{field}` must be a {}",
                        spec.event_kind,
                        field_type.json_type_name()
                    )));
                }
            } else {
                return Err(CalmError::BadRequest(format!(
                    "forge-action event_spec/context for `{}` must provide field `{field}`",
                    spec.event_kind
                )));
            }
        }
        for (field, source) in &spec.fields {
            if let FieldSource::JsonField { path } = source
                && !path.is_empty()
                && !path.starts_with('/')
            {
                return Err(CalmError::BadRequest(format!(
                    "forge-action event_spec field `{field}` JsonField path `{path}` must be a valid JSON Pointer (empty string or starting with `/`)"
                )));
            }
        }
        for (field, is_string) in required_output_fields(&spec.event_kind) {
            let Some(source) = spec.fields.get(*field) else {
                return Err(CalmError::BadRequest(format!(
                    "forge-action event_spec for `{}` must populate field `{}`",
                    spec.event_kind, field
                )));
            };
            if *is_string && !matches!(source, FieldSource::JsonField { .. }) {
                return Err(CalmError::BadRequest(format!(
                    "forge-action `{}` field `{}` must be a JSON string source, not {}",
                    spec.event_kind,
                    field,
                    field_source_type_name(source)
                )));
            }
        }
    }
    if payload.cwd_lease.as_os_str().is_empty() {
        return Err(CalmError::BadRequest(
            "forge-action cwd_lease must not be empty".into(),
        ));
    }
    if !payload.cwd_lease.is_absolute() {
        return Err(CalmError::BadRequest(
            "forge-action cwd_lease must be absolute".into(),
        ));
    }
    if payload.result_path.as_os_str().is_empty() {
        return Err(CalmError::BadRequest(
            "forge-action result_path must not be empty".into(),
        ));
    }
    if !payload.result_path.is_absolute() {
        return Err(CalmError::BadRequest(
            "forge-action result_path must be absolute".into(),
        ));
    }
    if payload.deadline_ms <= 0 {
        return Err(CalmError::BadRequest(
            "forge-action deadline_ms must be positive".into(),
        ));
    }
    if let Some(probe) = payload.probe.as_ref()
        && probe.probe_argv.is_empty()
    {
        return Err(CalmError::BadRequest(
            "forge-action probe.probe_argv must not be empty".into(),
        ));
    }
    if let Some(probe) = payload.probe.as_ref() {
        if let Some(output_probe_argv) = probe.output_probe_argv.as_ref()
            && output_probe_argv.is_empty()
        {
            return Err(CalmError::BadRequest(
                "forge-action probe.output_probe_argv must not be empty".into(),
            ));
        }
        if forge_spec_needs_json(payload.event_spec.as_ref()) && probe.output_probe_argv.is_none() {
            return Err(CalmError::BadRequest(
                "forge-action probe.output_probe_argv must be present when event_spec uses JsonField"
                    .into(),
            ));
        }
    }
    Ok(())
}

fn build_forge_event(
    frozen: &FrozenForge,
    exit_code: i32,
    stdout: &str,
) -> std::result::Result<(Option<Event>, Value), ForgeEventBuildError> {
    if exit_code != 0 {
        return Err(ForgeEventBuildError::ActionFailed {
            reason: format!("forge action exited with code {exit_code}"),
        });
    }
    let Some(spec) = frozen.event_spec.as_ref() else {
        return Ok((
            None,
            json!({
                "exit_code": exit_code,
                "event_kind": Value::Null,
                "event": Value::Null,
            }),
        ));
    };
    let json_stdout = parse_json_stdout_if_needed(Some(spec), stdout).map_err(|e| {
        ForgeEventBuildError::ExtractionFailed {
            reason: e.to_string(),
        }
    })?;
    let mut payload = frozen.context.clone();
    for (key, value) in spec
        .extract_payload(exit_code, json_stdout.as_ref())
        .map_err(|e| ForgeEventBuildError::ExtractionFailed {
            reason: format!("gate-infra: forge event extraction failed: {e}"),
        })?
    {
        payload.insert(key, value);
    }
    for field in kernel_injected_fields(&spec.event_kind) {
        if payload.contains_key(*field) {
            return Err(ForgeEventBuildError::ExtractionFailed {
                reason: format!("forge event context/output may not set reserved key `{field}`"),
            });
        }
    }
    for field in kernel_injected_fields(&spec.event_kind) {
        match *field {
            "wave_id" => {
                payload.insert(
                    "wave_id".into(),
                    serde_json::to_value(WaveId::from(frozen.wave_id.clone())).map_err(|e| {
                        ForgeEventBuildError::ExtractionFailed {
                            reason: format!(
                                "gate-infra: forge event context serialize failed: {e}"
                            ),
                        }
                    })?,
                );
            }
            "subject" if spec.event_kind == "forge.pr.merged" => {
                if let Some(subject) = frozen.subject.as_ref() {
                    payload.insert(
                        "subject".into(),
                        serde_json::to_value(subject).map_err(|e| {
                            ForgeEventBuildError::ExtractionFailed {
                                reason: format!(
                                    "gate-infra: forge event subject serialize failed: {e}"
                                ),
                            }
                        })?,
                    );
                }
            }
            "card_id" => {
                payload.insert(
                    "card_id".into(),
                    serde_json::to_value(CardId::from(frozen.card_id.clone())).map_err(|e| {
                        ForgeEventBuildError::ExtractionFailed {
                            reason: format!(
                                "gate-infra: forge event card_id serialize failed: {e}"
                            ),
                        }
                    })?,
                );
            }
            "artifact_path" if is_artifact_bearing_forge_event_kind(&spec.event_kind) => {
                payload.insert(
                    "artifact_path".into(),
                    Value::String(frozen.result_path.display().to_string()),
                );
            }
            _ => {}
        }
    }
    let payload_value = Value::Object(payload);
    let event =
        Event::from_kind_and_payload(&spec.event_kind, payload_value.clone()).map_err(|e| {
            ForgeEventBuildError::ExtractionFailed {
                reason: format!("gate-infra: forge event deserialize failed: {e}"),
            }
        })?;
    let mut result = Map::new();
    result.insert("exit_code".into(), json!(exit_code));
    result.insert("event_kind".into(), Value::String(spec.event_kind.clone()));
    result.insert("event".into(), payload_value);
    if spec.event_kind == "forge.issue.read" {
        result.insert("stdout".into(), Value::String(stdout.to_owned()));
    }
    Ok((Some(event), Value::Object(result)))
}

async fn read_result_file(result_path: &Path) -> Result<ForgeActionResultFile> {
    let code_path = result_code_path(result_path);
    let stdout_path = result_stdout_path(result_path);
    let code_text = tokio::fs::read_to_string(&code_path).await.map_err(|e| {
        CalmError::Internal(format!(
            "gate-infra: forge action result code {} unreadable: {e}",
            code_path.display()
        ))
    })?;
    let exit_code = code_text.trim().parse::<i32>().map_err(|e| {
        CalmError::Internal(format!(
            "gate-infra: forge action result code {} unparseable: {e}",
            code_path.display()
        ))
    })?;
    let stdout = match tokio::fs::read(&stdout_path).await {
        Ok(bytes) => String::from_utf8_lossy(&bytes).to_string(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => {
            return Err(CalmError::Internal(format!(
                "gate-infra: forge action result stdout {} unreadable: {e}",
                stdout_path.display()
            )));
        }
    };
    Ok(ForgeActionResultFile { exit_code, stdout })
}

fn is_artifact_bearing_forge_event_kind(event_kind: &str) -> bool {
    matches!(event_kind, "forge.pr.diff.read" | "forge.issue.read")
}

fn is_artifact_bearing_forge_event(frozen: &FrozenForge) -> bool {
    frozen
        .event_spec
        .as_ref()
        .is_some_and(|spec| is_artifact_bearing_forge_event_kind(&spec.event_kind))
}

async fn persist_forge_artifact_if_needed(
    frozen: &FrozenForge,
    exit_code: i32,
    stdout: &str,
) -> Result<()> {
    if exit_code != 0 || !is_artifact_bearing_forge_event(frozen) {
        return Ok(());
    }
    if let Some(parent) = frozen.result_path.parent()
        && !parent.as_os_str().is_empty()
    {
        tokio::fs::create_dir_all(parent).await?;
    }
    let tmp_path = forge_artifact_tmp_path(&frozen.result_path);
    tokio::fs::write(&tmp_path, stdout.as_bytes()).await?;
    tokio::fs::rename(&tmp_path, &frozen.result_path).await?;
    Ok(())
}

async fn complete_forge_op_failed(
    pool: &sqlx::SqlitePool,
    completion: &OperationCompletionBus,
    op_id: &str,
    reason: String,
    last_error_class: Option<String>,
) -> Result<()> {
    let outcome = ParkedOutcome::Failed {
        last_error: reason,
        last_error_class,
    };
    let mut tx = begin_immediate_tx(pool).await?;
    match complete_parked_tx(&mut tx, &op_id.to_string(), &outcome).await? {
        ParkedCompletion::Completed(result) => {
            tx.commit().await?;
            completion.complete(result);
        }
        ParkedCompletion::AlreadyResolved { phase } => {
            tx.rollback().await?;
            tracing::debug!(
                op_id,
                phase = ?phase,
                "forge observer: op already resolved; failure discarded"
            );
        }
    }
    Ok(())
}

pub(crate) async fn complete_forge_op_with_result(
    pool: &sqlx::SqlitePool,
    completion: &OperationCompletionBus,
    events: &EventBus,
    op_id: &str,
    frozen: &FrozenForge,
    exit_code: i32,
    stdout: &str,
) -> Result<()> {
    if let Err(e) = persist_forge_artifact_if_needed(frozen, exit_code, stdout).await {
        return complete_forge_op_failed(
            pool,
            completion,
            op_id,
            format!("gate-infra: forge artifact write failed: {e}"),
            Some("gate-infra".into()),
        )
        .await;
    }
    let (event, result) = match build_forge_event(frozen, exit_code, stdout) {
        Ok(ok) => ok,
        Err(ForgeEventBuildError::ActionFailed { reason }) => {
            return complete_forge_op_failed(
                pool,
                completion,
                op_id,
                reason,
                Some("action-failed".into()),
            )
            .await;
        }
        Err(ForgeEventBuildError::ExtractionFailed { reason }) => {
            return complete_forge_op_failed(
                pool,
                completion,
                op_id,
                reason,
                Some("gate-infra".into()),
            )
            .await;
        }
    };

    complete_forge_op_succeeded(pool, completion, events, op_id, frozen, event, result).await
}

async fn complete_forge_op_from_live_result(
    refs: ForgeCompletionRefs<'_>,
    op_id: &str,
    frozen: &FrozenForge,
    exit_code: i32,
    stdout: &str,
) -> Result<()> {
    if let Err(e) = persist_forge_artifact_if_needed(frozen, exit_code, stdout).await {
        return complete_forge_op_failed(
            refs.pool,
            refs.completion,
            op_id,
            format!("gate-infra: forge artifact write failed: {e}"),
            Some("gate-infra".into()),
        )
        .await;
    }
    let (event, result) = match build_forge_event(frozen, exit_code, stdout) {
        Ok(ok) => ok,
        Err(ForgeEventBuildError::ActionFailed { reason }) => {
            // Once the go-token is released, a nonzero exit is ambiguous,
            // not a verdict; the probe is authoritative for landed status.
            return resolve_post_release_via_probe(
                refs.pool,
                refs.completion,
                refs.events,
                refs.repo,
                op_id,
                frozen,
                &format!("action-failed: {reason}"),
            )
            .await;
        }
        Err(ForgeEventBuildError::ExtractionFailed { reason }) => {
            return resolve_post_release_via_probe(
                refs.pool,
                refs.completion,
                refs.events,
                refs.repo,
                op_id,
                frozen,
                &format!("extraction failed: {reason}"),
            )
            .await;
        }
    };

    complete_forge_op_succeeded(
        refs.pool,
        refs.completion,
        refs.events,
        op_id,
        frozen,
        event,
        result,
    )
    .await
}

async fn complete_forge_op_succeeded(
    pool: &sqlx::SqlitePool,
    completion: &OperationCompletionBus,
    events: &EventBus,
    op_id: &str,
    frozen: &FrozenForge,
    event: Option<Event>,
    result: Value,
) -> Result<()> {
    let outcome = ParkedOutcome::Succeeded { result };
    let mut tx = begin_immediate_tx(pool).await?;
    match complete_parked_tx(&mut tx, &op_id.to_string(), &outcome).await? {
        ParkedCompletion::Completed(result) => {
            let envelope = if let Some(event) = event {
                let scope = frozen.event_scope_for(event.kind_tag());
                let event_id = append_decision_event_in_tx(
                    &mut tx,
                    &PermissiveGate,
                    &ActorId::KernelDispatcher,
                    &scope,
                    None,
                    &event,
                )
                .await?;
                Some(BroadcastEnvelope {
                    id: event_id,
                    event_version: SYNC_EVENT_VERSION,
                    actor: ActorId::KernelDispatcher,
                    scope,
                    event,
                })
            } else {
                None
            };
            // #840 e2 crash seam. The whole block (including the `format!`
            // argument) is `#[cfg]`-gated on the test-only `fixtures` feature,
            // which production builds never enable — a release binary compiles
            // literally zero code here. In a fixtures build it fires only when
            // `CALM_TEST_CRASH_AT` matches the event-kind-qualified point
            // exactly (see `test_seams` for the full prod-safety contract); it
            // cannot alter control flow otherwise. Placed immediately before
            // the fence commit so a crash here proves the uncommitted fence
            // UPDATE and the appended decision event both vanish together
            // (exactly-once merge across a reboot).
            #[cfg(feature = "fixtures")]
            crate::test_seams::crash_point(&format!(
                "forge-pre-fence-commit:{}",
                envelope.as_ref().map_or("none", |e| e.event.kind_tag())
            ));
            tx.commit().await?;
            completion.complete(result);
            if let Some(envelope) = envelope {
                events.emit_envelope(envelope);
            }
        }
        ParkedCompletion::AlreadyResolved { phase } => {
            tx.rollback().await?;
            tracing::debug!(
                op_id,
                phase = ?phase,
                "forge observer: op already resolved; event discarded"
            );
        }
    }
    Ok(())
}

fn verdict_from_exit_code(exit_code: Option<i32>) -> ProbeVerdict {
    match exit_code {
        Some(0) => ProbeVerdict::Landed,
        Some(1) => ProbeVerdict::NotLanded,
        _ => ProbeVerdict::Unknown,
    }
}

async fn run_probe(
    argv: &[String],
    cwd: &Path,
    repo: &dyn RouteRepo,
) -> Result<(Option<i32>, String)> {
    if argv.is_empty() {
        return Err(CalmError::Internal(
            "forge-action probe argv must not be empty".into(),
        ));
    }
    let mut cmd = tokio::process::Command::new(&argv[0]);
    cmd.args(&argv[1..])
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    apply_forge_subprocess_env(&mut cmd, repo).await;
    let output = tokio::time::timeout(PROBE_TIMEOUT, cmd.output())
        .await
        .map_err(|_| CalmError::Internal("forge-action probe timed out".into()))??;
    Ok((
        output.status.code(),
        String::from_utf8_lossy(&output.stdout).to_string(),
    ))
}

async fn complete_from_probe(
    pool: &sqlx::SqlitePool,
    completion: &OperationCompletionBus,
    events: &EventBus,
    repo: &dyn RouteRepo,
    op_id: &str,
    frozen: &FrozenForge,
    probe: &ProbeSpec,
) -> Result<ParkedRecovery> {
    let (exit_code, _) = match run_probe(&probe.probe_argv, &frozen.cwd_lease, repo).await {
        Ok(result) => result,
        Err(e) => {
            return Ok(ParkedRecovery::Fail {
                reason: format!("forge action probe failed; gate-infra: {e}"),
            });
        }
    };
    match verdict_from_exit_code(exit_code) {
        ProbeVerdict::Landed => {
            let stdout = if forge_spec_needs_json(frozen.event_spec.as_ref()) {
                let Some(output_probe_argv) = probe.output_probe_argv.as_ref() else {
                    return Ok(ParkedRecovery::Fail {
                        reason:
                            "forge action output probe missing for JSON re-extraction; gate-infra"
                                .into(),
                    });
                };
                match run_probe(output_probe_argv, &frozen.cwd_lease, repo).await {
                    Ok((Some(0), stdout)) => stdout,
                    Ok((exit_code, _)) => {
                        return Ok(ParkedRecovery::Fail {
                            reason: format!(
                                "forge action output probe failed with exit code {exit_code:?}; gate-infra"
                            ),
                        });
                    }
                    Err(e) => {
                        return Ok(ParkedRecovery::Fail {
                            reason: format!("forge action output probe failed; gate-infra: {e}"),
                        });
                    }
                }
            } else {
                String::new()
            };
            complete_forge_op_with_result(pool, completion, events, op_id, frozen, 0, &stdout)
                .await?;
            Ok(ParkedRecovery::LeaveParked)
        }
        ProbeVerdict::NotLanded => Ok(ParkedRecovery::Fail {
            reason: "forge action process dead and probe reports not landed".into(),
        }),
        ProbeVerdict::Unknown => Ok(ParkedRecovery::Fail {
            reason: "forge action probe verdict unknown; gate-infra".into(),
        }),
    }
}

/// Resolve an ambiguous post-release outcome. Once the go-token has been
/// released, the probe is authoritative for whether the irreversible action
/// landed; gate-infra is only terminal when no probe exists or the probe cannot
/// produce a landed/not-landed verdict. If `complete_from_probe` returns `Err`,
/// the probe has already reported `Landed`; only the typed-event completion tx
/// failed, so the parked row must remain available for a later retry.
async fn resolve_post_release_via_probe(
    pool: &sqlx::SqlitePool,
    completion: &OperationCompletionBus,
    events: &EventBus,
    repo: &dyn RouteRepo,
    op_id: &str,
    frozen: &FrozenForge,
    ambiguous_reason: &str,
) -> Result<()> {
    let Some(probe) = frozen.probe.as_ref() else {
        if let Some(reason) = ambiguous_reason.strip_prefix("action-failed: ") {
            let _ = complete_forge_op_failed(
                pool,
                completion,
                op_id,
                reason.to_string(),
                Some("action-failed".into()),
            )
            .await;
            return Ok(());
        }
        let _ = complete_forge_op_failed(
            pool,
            completion,
            op_id,
            format!("gate-infra: {ambiguous_reason}; no probe to resolve outcome"),
            Some("gate-infra".into()),
        )
        .await;
        return Ok(());
    };

    match complete_from_probe(pool, completion, events, repo, op_id, frozen, probe).await {
        Ok(ParkedRecovery::Fail { reason }) => {
            let last_error_class = if reason.contains("gate-infra") {
                "gate-infra"
            } else {
                "action-not-landed"
            };
            let _ = complete_forge_op_failed(
                pool,
                completion,
                op_id,
                reason,
                Some(last_error_class.into()),
            )
            .await;
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

/// Resolve a dead forge process post-release: prefer the durable result files
/// (authoritative — written by the wrapper via tmp+rename only after the
/// action completed); fall back to the plugin probe; fail only if neither can
/// answer. Self-completes the op via the parked first-committer-wins fence.
async fn resolve_dead_outcome(
    pool: &sqlx::SqlitePool,
    completion: &OperationCompletionBus,
    events: &EventBus,
    repo: &dyn RouteRepo,
    op_id: &str,
    frozen: &FrozenForge,
    ambiguous_reason: &str,
) {
    if let Ok(result) = read_result_file(&frozen.result_path).await {
        if let Err(e) = complete_forge_op_from_live_result(
            ForgeCompletionRefs {
                pool,
                completion,
                events,
                repo,
            },
            op_id,
            frozen,
            result.exit_code,
            &result.stdout,
        )
        .await
        {
            tracing::error!(
                op_id,
                error = %e,
                "forge: result-file completion tx failed; falling back to probe"
            );
        } else {
            return;
        }
    }
    if let Err(e) = resolve_post_release_via_probe(
        pool,
        completion,
        events,
        repo,
        op_id,
        frozen,
        ambiguous_reason,
    )
    .await
    {
        tracing::error!(
            op_id,
            error = %e,
            "forge: landed-verdict completion tx failed during dead recovery; leaving op parked"
        );
    }
}

#[async_trait]
impl ProviderAdapter for ForgeActionAdapter {
    fn kind(&self) -> &'static str {
        FORGE_ACTION_KIND
    }

    fn phases(&self) -> &'static [PhaseTag] {
        FORGE_ACTION_PHASES
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: ForgeActionPayload = serde_json::from_value(input.clone())?;
        validate_payload(&payload)
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        op: &Operation,
    ) -> Result<TxOutput> {
        let payload: ForgeActionPayload = serde_json::from_value(input.clone())?;
        validate_payload(&payload)?;
        if !operation_key_matches_payload_idem(
            op.idempotency_key.as_deref(),
            &payload.wave_id,
            &payload.card_id,
            &payload.idem_key,
        ) {
            return Err(CalmError::BadRequest(
                "forge-action idempotency key does not match payload idem_key".into(),
            ));
        }

        let cove_id: String = sqlx::query_scalar("SELECT cove_id FROM waves WHERE id = ?1")
            .bind(&payload.wave_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("wave {}", payload.wave_id)))?;

        let frozen = FrozenForge {
            wave_id: payload.wave_id,
            cove_id,
            card_id: payload.card_id,
            subject: payload.subject,
            argv: payload.argv,
            idem_key: payload.idem_key,
            event_spec: payload.event_spec,
            context: payload.context,
            probe: payload.probe,
            cwd_lease: payload.cwd_lease,
            result_path: payload.result_path,
            deadline_ms: payload.deadline_ms,
        };
        let mut output = TxOutput::new("wave", Some(frozen.wave_id.clone()), json!({}));
        output.data = serde_json::to_value(&frozen)?;
        Ok(output)
    }

    async fn app_server_interact(
        &self,
        _output: &mut TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        let frozen = FrozenForge::from_output(output)?;
        if let Some(artifacts) = &op.spawn_artifacts {
            kill_artifacts_group(artifacts);
        }

        if let Some(parent) = frozen.result_path.parent()
            && !parent.as_os_str().is_empty()
        {
            tokio::fs::create_dir_all(parent).await?;
        }
        remove_stale_result_files(&frozen.result_path).await?;
        if !frozen.cwd_lease.is_dir() {
            return Err(CalmError::Internal(format!(
                "forge-action cwd_lease {} does not exist",
                frozen.cwd_lease.display()
            )));
        }

        let wrapper = render_forge_wrapper(&frozen.argv)?;
        let mut cmd = tokio::process::Command::new("/bin/sh");
        cmd.arg("-c")
            .arg(wrapper)
            .current_dir(&frozen.cwd_lease)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        apply_forge_subprocess_env(&mut cmd, ctx.repo.as_ref()).await;
        cmd.env("NEIGE_FORGE_RESULT_PATH", &frozen.result_path);
        unsafe {
            cmd.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = cmd.spawn()?;
        let pid = child.id().map(|p| p as i32).ok_or_else(|| {
            CalmError::Internal("forge wrapper exited before pid could be read".into())
        })?;
        let pgid = pid;
        let start_time = read_proc_start_time(pid).ok_or_else(|| {
            CalmError::Internal(format!("forge wrapper pid {pid}: starttime unreadable"))
        })?;
        let boot_id =
            read_boot_id().ok_or_else(|| CalmError::Internal("boot_id unreadable".into()))?;
        let artifacts = SpawnArtifacts {
            pid,
            pgid,
            start_time,
            boot_id,
            log_path: None,
            extra: json!({
                "result_path": frozen.result_path.display().to_string(),
            }),
        };
        ctx.record_spawn_artifacts(op, &artifacts).await?;

        let pool = ctx.operation_repo.sqlite_pool();
        let completion = ctx.completion.clone();
        let events = ctx.events.clone();
        let op_id = op.id.clone();
        let observer_frozen = frozen.clone();
        let observer_artifacts = artifacts.clone();
        let observer_repo = ctx.repo.clone();
        let observer = Box::pin(async move {
            // #840 e3 crash seam. The whole statement (including the `format!`
            // argument) is `#[cfg]`-gated on the test-only `fixtures` feature,
            // which production builds never enable — a release binary compiles
            // literally zero code here. In a fixtures build it fires only when
            // `CALM_TEST_CRASH_AT` matches the event-kind-qualified point
            // exactly (see `test_seams` for the full prod-safety contract); it
            // cannot alter control flow otherwise. Placed as the FIRST
            // statement of the observer future — the driver spawns this task
            // only after `set_parked` commits, and the go-token `write_all`
            // below is the only thing that ever releases the held wrapper —
            // so an abort here freezes the exact danger-point-3 window: op
            // durably parked + wrapper spawned (spawn artifacts recorded) +
            // go token NOT yet written. The wrapper's `read -r _go` then hits
            // EOF at kernel death and exits 75 without ever running gh.
            #[cfg(feature = "fixtures")]
            crate::test_seams::crash_point(&format!(
                "forge-pre-go-token:{}",
                observer_frozen
                    .event_spec
                    .as_ref()
                    .map_or("none", |s| s.event_kind.as_str())
            ));
            let release = async {
                let mut stdin = child.stdin.take().ok_or_else(|| {
                    CalmError::Internal("forge wrapper stdin handle missing".into())
                })?;
                stdin
                    .write_all(b"go\n")
                    .await
                    .map_err(|e| CalmError::Internal(format!("forge release write failed: {e}")))?;
                drop(stdin);
                Ok::<(), CalmError>(())
            };
            match tokio::time::timeout(RELEASE_TIMEOUT, release).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    kill_artifacts_group(&observer_artifacts);
                    let _ = complete_forge_op_failed(
                        &pool,
                        &completion,
                        &op_id,
                        e.to_string(),
                        Some("gate-infra".into()),
                    )
                    .await;
                    return;
                }
                Err(_) => {
                    kill_artifacts_group(&observer_artifacts);
                    let _ = complete_forge_op_failed(
                        &pool,
                        &completion,
                        &op_id,
                        "gate-infra: forge release write did not complete within 60s".into(),
                        Some("gate-infra".into()),
                    )
                    .await;
                    return;
                }
            }

            // The parked-deadline sweep owns timeouts; the parked-phase
            // first-committer-wins guard makes any late observer completion roll back.
            match child.wait().await {
                Ok(status) if status.code().is_some() => {
                    match read_result_file(&observer_frozen.result_path).await {
                        Ok(result) => {
                            if let Err(e) = complete_forge_op_from_live_result(
                                ForgeCompletionRefs {
                                    pool: &pool,
                                    completion: &completion,
                                    events: &events,
                                    repo: observer_repo.as_ref(),
                                },
                                &op_id,
                                &observer_frozen,
                                result.exit_code,
                                &result.stdout,
                            )
                            .await
                            {
                                tracing::error!(
                                    op_id = %op_id,
                                    error = %e,
                                    "forge observer: completion tx failed; sweep/reconcile will recover"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::warn!(
                                op_id = %op_id,
                                error = %e,
                                "forge observer: result file unreadable after post-release wait"
                            );
                            if let Err(e) = resolve_post_release_via_probe(
                                &pool,
                                &completion,
                                &events,
                                observer_repo.as_ref(),
                                &op_id,
                                &observer_frozen,
                                "result file unreadable",
                            )
                            .await
                            {
                                tracing::error!(
                                    op_id = %op_id,
                                    error = %e,
                                    "forge observer: landed-verdict completion tx failed; sweep/reconcile will recover"
                                );
                            }
                        }
                    }
                }
                Ok(_) => {
                    if let Err(e) = resolve_post_release_via_probe(
                        &pool,
                        &completion,
                        &events,
                        observer_repo.as_ref(),
                        &op_id,
                        &observer_frozen,
                        "forge wrapper killed by signal",
                    )
                    .await
                    {
                        tracing::error!(
                            op_id = %op_id,
                            error = %e,
                            "forge observer: landed-verdict completion tx failed; sweep/reconcile will recover"
                        );
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        op_id = %op_id,
                        error = %e,
                        "forge observer: wrapper wait failed"
                    );
                    if let Err(e) = resolve_post_release_via_probe(
                        &pool,
                        &completion,
                        &events,
                        observer_repo.as_ref(),
                        &op_id,
                        &observer_frozen,
                        "wrapper wait failed",
                    )
                    .await
                    {
                        tracing::error!(
                            op_id = %op_id,
                            error = %e,
                            "forge observer: landed-verdict completion tx failed; sweep/reconcile will recover"
                        );
                    }
                }
            }
        });

        Ok(SpawnOutcome::Parked {
            deadline_ms: frozen.deadline_ms,
            observer,
        })
    }

    async fn recover_parked(
        &self,
        op: &Operation,
        artifacts: &SpawnArtifacts,
        alive: bool,
        mode: RecoveryMode,
        ctx: &SpawnCtx,
    ) -> Result<ParkedRecovery> {
        let frozen = op
            .tx_output
            .as_ref()
            .ok_or_else(|| CalmError::Internal("forge-action op missing tx_output".into()))
            .and_then(FrozenForge::from_output)?;
        if alive && verify_owned_pid(artifacts.pid, artifacts.start_time, &artifacts.boot_id) {
            return match mode {
                RecoveryMode::Boot => {
                    let pool = ctx.operation_repo.sqlite_pool();
                    let completion = ctx.completion.clone();
                    let events = ctx.events.clone();
                    let repo = ctx.repo.clone();
                    let op_id = op.id.clone();
                    let artifacts = artifacts.clone();
                    tokio::spawn(async move {
                        loop {
                            if !verify_owned_pid(
                                artifacts.pid,
                                artifacts.start_time,
                                &artifacts.boot_id,
                            ) {
                                break;
                            }
                            tokio::time::sleep(REATTACH_POLL).await;
                        }
                        resolve_dead_outcome(
                            &pool,
                            &completion,
                            &events,
                            repo.as_ref(),
                            &op_id,
                            &frozen,
                            "forge action process dead",
                        )
                        .await;
                    });
                    Ok(ParkedRecovery::LeaveParked)
                }
                RecoveryMode::PreDeadlineProbe => Ok(ParkedRecovery::LeaveParked),
                RecoveryMode::PastDeadline => Ok(ParkedRecovery::Fail {
                    reason: "action-timeout".into(),
                }),
            };
        }

        // P2-1: the durable result files are authoritative — the wrapper writes
        // <result_path>.code (tmp+rename) only after the action ran to completion.
        // A landed-but-dead action whose observer never committed is recovered from
        // them, even with probe:None. read failure (missing/torn .code) falls through
        // to the existing probe / no-probe path.
        if let Ok(result) = read_result_file(&frozen.result_path).await {
            // Propagate completion tx errors here so the op stays parked for a later sweep;
            // async dead reattach logs that error and falls back to the probe.
            let pool = ctx.operation_repo.sqlite_pool();
            complete_forge_op_from_live_result(
                ForgeCompletionRefs {
                    pool: &pool,
                    completion: &ctx.completion,
                    events: &ctx.events,
                    repo: ctx.repo.as_ref(),
                },
                &op.id,
                &frozen,
                result.exit_code,
                &result.stdout,
            )
            .await?;
            return Ok(ParkedRecovery::LeaveParked);
        }

        // Dead process: the action's process is gone; the plugin probe is the ONLY
        // truth for whether the irreversible action landed, so run it regardless of
        // the deadline -- a dead-but-landed action past deadline MUST still emit its
        // typed event (exactly-once recovery). Fail only when no probe is available.
        if frozen.probe.is_none() {
            return Ok(ParkedRecovery::Fail {
                reason: match mode {
                    RecoveryMode::PastDeadline => "action-timeout".into(),
                    _ => "forge action process dead with no probe; gate-infra".into(),
                },
            });
        }
        resolve_post_release_via_probe(
            &ctx.operation_repo.sqlite_pool(),
            &ctx.completion,
            &ctx.events,
            ctx.repo.as_ref(),
            &op.id,
            &frozen,
            "forge action process dead",
        )
        .await?;
        Ok(ParkedRecovery::LeaveParked)
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        _output: &TxOutput,
        op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        let artifacts = op
            .spawn_artifacts
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?
            .unwrap_or(Value::Null);
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps: vec![CompensationStep::new(
                "kill_forge_action_group",
                json!({ "artifacts": artifacts }),
            )],
        })
    }

    async fn compensate_step(
        &self,
        step: &CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<()> {
        if step.completed {
            return Ok(());
        }
        match step.op.as_str() {
            "kill_forge_action_group" => {
                if let Some(artifacts) = step.args.get("artifacts").filter(|v| !v.is_null()) {
                    let artifacts: SpawnArtifacts = serde_json::from_value(artifacts.clone())?;
                    kill_artifacts_group(&artifacts);
                }
                Ok(())
            }
            other => Err(CalmError::Internal(format!(
                "forge-action unknown compensation step {other}"
            ))),
        }
    }
}

fn operation_key_matches_payload_idem(
    op_idem_key: Option<&str>,
    wave_id: &str,
    card_id: &str,
    payload_idem_key: &str,
) -> bool {
    let Some(op_idem_key) = op_idem_key else {
        return false;
    };
    if op_idem_key == payload_idem_key {
        return true;
    }
    let scoped_suffix = format!(":{wave_id}:{card_id}:{payload_idem_key}");
    op_idem_key.ends_with(&scoped_suffix)
}
