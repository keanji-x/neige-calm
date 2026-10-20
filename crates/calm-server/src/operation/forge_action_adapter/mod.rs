//! Forge's Engine consumer. Only a claimed post-park observer releases the credential
//! payload; the durable owner record retains action and probe execution ownership.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use super::forge_execution as execution;
use super::{
    AppServerInteractOutcome, CompensationStateVersioned, CompensationStep, Operation, OwnerEvent,
    ParkedOutcome, PhaseTag, ProviderAdapter, SpawnArtifacts, SpawnCtx, SpawnOutcome, Tx, TxOutput,
};
use crate::db::RouteRepo;
use crate::error::{CalmError, Result};
use crate::event::{Event, EventScope, FieldSource, ForgeEventSpec, ForgeMergeSubject};
use crate::ids::{ActorId, AreaId, CardId, TrackId};
mod lifecycle;
mod observers;

#[cfg(feature = "fixtures")]
#[doc(hidden)]
pub fn observer_count_for_test(id: &str) -> usize {
    observers::live_count(id)
}

pub const FORGE_ACTION_KIND: &str = "forge-action";

/// The `last_error_class` values of a forge action that ran and did not land: it exited non-zero,
/// or its probe answered not landed. Any other failure leaves whether it landed unknown.
pub(crate) const NOT_LANDED_ERROR_CLASSES: &[&str] = &["action-failed", "action-not-landed"];

/// validate_payload rejects any other event_kind BEFORE the irreversible action can run, so a typo'd kind can never execute the side effect and then fail to record its event.
pub use calm_types::forge_action::SUPPORTED_FORGE_EVENT_KINDS;

static NEXT_FORGE_ARTIFACT_TMP: AtomicU64 = AtomicU64::new(1);
/// What a forge child inherits on top of the base allowlist (`PATH`, `HOME`).
const FORGE_EXTRA_ENV_KEYS: [&str; 3] = ["LANG", "LC_ALL", "TERM"];
pub use calm_types::forge_env::{
    FORGE_CREDENTIAL_ENV_KEYS, FORGE_NONCREDENTIAL_ENV_KEYS, forge_passthrough_env_keys,
};

const FORGE_ACTION_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::SpawnStarted,
    PhaseTag::Parked,
    PhaseTag::Succeeded,
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForgeActionPayload {
    pub track_id: String,
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

/// The event a forge-action operation emits on success, as its pending receipt names it.
pub(crate) fn completion_event(op: &Operation) -> Result<Option<String>> {
    let payload: ForgeActionPayload = serde_json::from_value(op.payload.clone())?;
    Ok(payload.event_spec.map(|event| event.event_kind))
}

pub use calm_types::forge_action::ProbeSpec;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct FrozenForge {
    pub track_id: String,
    pub area_id: String,
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
                track: TrackId::from(self.track_id.clone()),
                area: AreaId::from(self.area_id.clone()),
            }
        } else {
            EventScope::Track {
                track: TrackId::from(self.track_id.clone()),
                area: AreaId::from(self.area_id.clone()),
            }
        }
    }
}

#[doc(hidden)]
pub mod credentials;

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

/// `<result_path>.code` and `<result_path>.stdout` as the forge wrapper left them; the delivery
/// settlement (`git_candidate::delivery::classify_failure`) reads the exit code and the evidence lines.
#[derive(Clone, Debug, Deserialize)]
pub(crate) struct ForgeActionResultFile {
    pub(crate) exit_code: i32,
    pub(crate) stdout: String,
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

/// POSIX single-quote escaping: `'` -> `'\''`.
fn sh_single_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[doc(hidden)]
pub fn render_forge_wrapper(argv: &[String]) -> Result<String> {
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
    let mut script = String::from("exec >/dev/null 2>&1\n");
    for key in FORGE_CREDENTIAL_ENV_KEYS {
        script.push_str(&format!(
            "IFS= read -r neige_forge_credential || exit 75\n\
             case \"$neige_forge_credential\" in\n\
             0) unset {key} ;;\n\
             1*) export {key}=\"${{neige_forge_credential#1}}\" ;;\n\
             *) exit 75 ;;\n\
             esac\n"
        ));
    }
    script.push_str("unset neige_forge_credential\n");
    script.push_str("IFS= read -r neige_forge_go || exit 75\n");
    script.push_str("[ \"$neige_forge_go\" = go ] || exit 75\n");
    script.push_str("unset neige_forge_go\n");
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

pub(crate) async fn remove_stale_result_files(result_path: &Path) -> Result<()> {
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

#[cfg(any(test, feature = "fixtures"))]
fn forge_passthrough_env_from<F>(mut lookup: F) -> Vec<(&'static str, std::ffi::OsString)>
where
    F: FnMut(&str) -> Option<std::ffi::OsString>,
{
    forge_passthrough_env_keys()
        .filter_map(|key| lookup(key).map(|value| (key, value)))
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

/// `env_clear` + the base allowlist alone (no forge passthrough, no worker env): the environment
/// every kernel-side git observation runs under — the forge action and its probes below, and the
/// task-verify target sampler (#1727 S4 D3.0), which must resolve `git` from the same `PATH`.
pub(crate) fn forge_base_env(cmd: &mut tokio::process::Command) {
    cmd.env_clear()
        .envs(crate::plugin_host::child_process::inherited_env(
            &FORGE_EXTRA_ENV_KEYS,
        ));
}

/// The same allowlist for every forge intent; credentials enter only through release stdin.
pub(crate) async fn forge_environment(repo: &dyn RouteRepo) -> Result<Vec<(String, String)>> {
    let mut env = crate::plugin_host::child_process::inherited_env(&FORGE_EXTRA_ENV_KEYS)
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string_lossy().into_owned()))
        .collect::<Vec<_>>();
    if let Value::Object(worker) = super::terminal_adapter::terminal_worker_env(repo).await? {
        env.extend(
            worker
                .into_iter()
                .filter_map(|(k, v)| v.as_str().map(|v| (k, v.into()))),
        );
    }
    env.extend(
        FORGE_NONCREDENTIAL_ENV_KEYS
            .iter()
            .filter_map(|key| std::env::var(key).ok().map(|v| ((*key).into(), v))),
    );
    env.retain(|(key, _)| !FORGE_CREDENTIAL_ENV_KEYS.contains(&key.as_str()));
    Ok(env)
}

mod event_policy;
use event_policy::*;

pub(crate) async fn read_result_file(result_path: &Path) -> Result<ForgeActionResultFile> {
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
    matches!(
        event_kind,
        "forge.pr.diff.read"
            | "forge.ci.evidence.read"
            | "forge.issue.read"
            | "forge.issue.searched"
    )
}

fn is_artifact_bearing_forge_event(frozen: &FrozenForge) -> bool {
    frozen
        .event_spec
        .as_ref()
        .is_some_and(|event_spec| is_artifact_bearing_forge_event_kind(&event_spec.event_kind))
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

/// Verdict, artifact references and owner event facts have no transactional dependencies.
fn forge_result_outcome(
    frozen: &FrozenForge,
    exit_code: i32,
    stdout: &str,
) -> (ParkedOutcome, Option<OwnerEvent>) {
    match build_forge_event(frozen, exit_code, stdout) {
        Ok((event, result)) => (
            ParkedOutcome::Succeeded { result },
            event.map(|event| OwnerEvent {
                actor: ActorId::KernelDispatcher,
                scope: frozen.event_scope_for(event.kind_tag()),
                event,
            }),
        ),
        Err(error) => {
            let (last_error, class) = match error {
                ForgeEventBuildError::ActionFailed { reason } => (reason, "action-failed"),
                ForgeEventBuildError::ExtractionFailed { reason } => (reason, "gate-infra"),
            };
            (
                ParkedOutcome::Failed {
                    last_error,
                    last_error_class: Some(class.into()),
                },
                None,
            )
        }
    }
}

fn verdict_from_exit_code(exit_code: Option<i32>) -> ProbeVerdict {
    match exit_code {
        Some(0) => ProbeVerdict::Landed,
        Some(1) => ProbeVerdict::NotLanded,
        _ => ProbeVerdict::Unknown,
    }
}

#[async_trait]
impl ProviderAdapter for ForgeActionAdapter {
    async fn preparation_track(&self, input: &Value, _ctx: &SpawnCtx) -> Result<Option<String>> {
        Ok(Some(
            serde_json::from_value::<ForgeActionPayload>(input.clone())?.track_id,
        ))
    }

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
            &payload.track_id,
            &payload.card_id,
            &payload.idem_key,
        ) {
            return Err(CalmError::BadRequest(
                "forge-action idempotency key does not match payload idem_key".into(),
            ));
        }

        let area_id: String = sqlx::query_scalar("SELECT area_id FROM tracks WHERE id = ?1")
            .bind(&payload.track_id)
            .fetch_optional(&mut **tx)
            .await?
            .ok_or_else(|| CalmError::NotFound(format!("track {}", payload.track_id)))?;

        let frozen = FrozenForge {
            track_id: payload.track_id,
            area_id,
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
        let mut output = TxOutput::new("track", Some(frozen.track_id.clone()), json!({}));
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

    fn owns_parked_resource(&self) -> Option<&'static str> {
        Some(execution::KEY)
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnOutcome> {
        let frozen = FrozenForge::from_output(output)?;
        let redrive = output.data.get(execution::KEY).is_some();
        if op.spawn_artifacts.is_some() {
            return Err(lifecycle::legacy_error(op));
        }
        if redrive {
            let record = execution::record(op)?;
            if record.release == execution::Release::Held
                && record.decision == execution::Decision::Observing
            {
                execution::stop(
                    ctx,
                    op,
                    execution::Kind::Action,
                    execution::StopReason::KernelRestarted,
                )
                .await?;
            }
        } else {
            execution::engine(ctx)?;
            if let Some(parent) = frozen.result_path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            execution::admit_action(ctx, op, &frozen).await?;
        }
        let (ctx, id) = (ctx.clone(), op.id.clone());
        Ok(SpawnOutcome::Parked {
            deadline_ms: frozen.deadline_ms,
            observer: Box::pin(async move {
                if let Some(observer) = observers::ensure_observer(ctx, id) {
                    let _ = observer.await;
                }
            }),
        })
    }

    async fn recover_owned_parked(
        &self,
        op: &Operation,
        mode: super::RecoveryMode,
        ctx: &SpawnCtx,
    ) -> Result<super::ParkedRecovery> {
        let outcome = lifecycle::recover(op, mode, ctx).await?;
        if matches!(outcome, super::ParkedRecovery::LeaveParked) {
            // Every recovery that leaves work parked restores its accelerator after
            // the committed step. The sweep guarantees liveness if an observer is lost.
            observers::ensure_observer(ctx.clone(), op.id.clone());
        }
        Ok(outcome)
    }

    async fn complete_owned_parked_tx(
        &self,
        tx: &mut Tx<'_>,
        op: &Operation,
    ) -> Result<Vec<crate::event::BroadcastEnvelope>> {
        execution::retire(tx, op).await?;
        #[cfg(feature = "fixtures")]
        crate::test_seams::crash_point(&format!(
            "forge-pre-fence-commit:{}",
            FrozenForge::from_output(
                op.tx_output
                    .as_ref()
                    .ok_or_else(|| CalmError::Internal("missing forge output".into()))?
            )?
            .event_spec
            .as_ref()
            .map_or("none", |s| s.event_kind.as_str())
        ));
        Ok(Vec::new())
    }

    fn owned_parked_completed(&self, op: &Operation, ctx: &SpawnCtx) {
        if let Ok(record) = execution::record(op) {
            ctx.execution.drive(record.ids());
        }
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
                    return Err(CalmError::Conflict(format!(
                        "drain under the old kernel or manually stop pgid {}",
                        artifacts.pgid
                    )));
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
    track_id: &str,
    card_id: &str,
    payload_idem_key: &str,
) -> bool {
    let Some(op_idem_key) = op_idem_key else {
        return false;
    };
    if op_idem_key == payload_idem_key {
        return true;
    }
    let scoped_suffix = format!(":{track_id}:{card_id}:{payload_idem_key}");
    op_idem_key.ends_with(&scoped_suffix)
}
