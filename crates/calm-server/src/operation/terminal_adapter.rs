use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::card_role_cache::CardRoleCache;
use crate::db::sqlite::{card_with_terminal_create_tx, event_append_for_operation_tx};
use crate::error::{CalmError, Result};
use crate::event::{BroadcastEnvelope, Event, SYNC_EVENT_VERSION};
use crate::ids::{ActorId, CardId, WaveId};
use crate::model::{CardRole, new_id};
use crate::routes::cards::card_scope;
use crate::routes::theme::RequestTheme;
use crate::terminal_sweeper::reap_terminal_artifacts_with_renderer;
use crate::wave_cove_cache::WaveCoveCache;

use super::{
    AppServerInteractOutcome, CompensationStateVersioned, CompensationStep, Operation, PhaseTag,
    ProviderAdapter, SpawnCtx, SpawnHandle, Tx, TxOutput,
};

#[cfg(feature = "fixtures")]
use futures::future::BoxFuture;

#[cfg(feature = "fixtures")]
type SpawnHook = Arc<
    dyn Fn(String, String, String, Value) -> BoxFuture<'static, Result<SpawnHandle>> + Send + Sync,
>;

const TERMINAL_PHASES: &[PhaseTag] = &[
    PhaseTag::Pending,
    PhaseTag::TxCommitted,
    PhaseTag::SpawnStarted,
    PhaseTag::SpawnSucceeded,
    PhaseTag::Succeeded,
];

#[derive(Clone)]
pub struct TerminalAdapter {
    repo: Arc<dyn crate::db::RouteRepo>,
    card_role_cache: CardRoleCache,
    wave_cove_cache: WaveCoveCache,
    #[cfg(feature = "fixtures")]
    spawn_hook: Option<SpawnHook>,
}

impl TerminalAdapter {
    pub fn new(
        repo: Arc<dyn crate::db::RouteRepo>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
    ) -> Self {
        Self {
            repo,
            card_role_cache,
            wave_cove_cache,
            #[cfg(feature = "fixtures")]
            spawn_hook: None,
        }
    }

    #[cfg(feature = "fixtures")]
    pub fn new_with_spawn_hook(
        repo: Arc<dyn crate::db::RouteRepo>,
        card_role_cache: CardRoleCache,
        wave_cove_cache: WaveCoveCache,
        spawn_hook: SpawnHook,
    ) -> Self {
        Self {
            repo,
            card_role_cache,
            wave_cove_cache,
            spawn_hook: Some(spawn_hook),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminalCreateOperationPayload {
    pub actor: ActorId,
    #[serde(flatten)]
    pub request: TerminalCreateRequestPayload,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminalCreateRequestPayload {
    pub wave_id: String,
    #[serde(default)]
    pub sort: Option<f64>,
    #[serde(default)]
    pub program: String,
    #[serde(default)]
    pub cwd: String,
    #[serde(default)]
    pub env: Value,
    pub theme: RequestTheme,
}

pub fn normalize_terminal_create_request(
    mut request: TerminalCreateRequestPayload,
) -> TerminalCreateRequestPayload {
    request.program = normalize_program(request.program);
    request.cwd = normalize_cwd(request.cwd);
    request.env = normalize_env(request.env);
    request
}

#[async_trait]
impl ProviderAdapter for TerminalAdapter {
    fn kind(&self) -> &'static str {
        "terminal-create"
    }

    fn phases(&self) -> &'static [PhaseTag] {
        TERMINAL_PHASES
    }

    async fn validate(&self, input: &Value) -> Result<()> {
        let payload: TerminalCreateOperationPayload = serde_json::from_value(input.clone())?;
        if self
            .repo
            .wave_get(&payload.request.wave_id)
            .await?
            .is_none()
        {
            return Err(CalmError::NotFound(format!(
                "wave {}",
                payload.request.wave_id
            )));
        }
        Ok(())
    }

    async fn prepare_tx<'tx>(
        &self,
        tx: &mut Tx<'tx>,
        input: &Value,
        _op: &Operation,
    ) -> Result<TxOutput> {
        let payload: TerminalCreateOperationPayload = serde_json::from_value(input.clone())?;
        let program = payload.request.program.clone();
        let cwd = payload.request.cwd.clone();
        let env = payload.request.env.clone();
        let card_id = new_id();
        let wave_id = payload.request.wave_id.clone();
        let scope = card_scope(
            self.repo.as_ref(),
            CardId::from(card_id.clone()),
            WaveId::from(wave_id.clone()),
        )
        .await?;
        let (card, term) = card_with_terminal_create_tx(
            tx,
            card_id,
            WaveId::from(wave_id),
            payload.request.sort,
            program.clone(),
            cwd.clone(),
            env.clone(),
            CardRole::Plain,
            true,
            &self.card_role_cache,
            payload.request.theme,
        )
        .await?;
        let event = Event::CardAdded(card.clone());
        if let Err(violation) = crate::role_gate::enforce_role(
            &payload.actor,
            &event,
            &scope,
            &self.card_role_cache,
            &self.wave_cove_cache,
        ) {
            return Err(CalmError::Forbidden(violation.to_string()));
        }
        let event_id =
            event_append_for_operation_tx(tx, &payload.actor, &scope, None, &event).await?;

        let mut output = TxOutput::new(
            "card",
            Some(card.id.to_string()),
            serde_json::to_value(&card)?,
        );
        output.data = json!({
            "card_id": card.id,
            "terminal_id": term.id,
            "program": program,
            "cwd": cwd,
            "env": env,
        });
        output.post_commit_events.push(BroadcastEnvelope {
            id: event_id,
            event_version: SYNC_EVENT_VERSION,
            actor: payload.actor,
            scope,
            event,
        });
        Ok(output)
    }

    async fn app_server_interact(
        &self,
        _output: &TxOutput,
        _op: &Operation,
        _ctx: &SpawnCtx,
    ) -> Result<AppServerInteractOutcome> {
        Ok(AppServerInteractOutcome::NotApplicable)
    }

    async fn spawn_side_effect(
        &self,
        output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<SpawnHandle> {
        let terminal_id = output_string(output, "terminal_id")?;
        let program = output_string(output, "program")?;
        let cwd = output_string(output, "cwd")?;
        let env = output.data.get("env").cloned().unwrap_or_else(|| json!({}));
        ctx.repo.terminal_clear_exit_for_spawn(&terminal_id).await?;
        let term = ctx
            .repo
            .terminal_get(&terminal_id)
            .await?
            .ok_or_else(|| CalmError::Internal(format!("terminal {terminal_id} vanished")))?;

        #[cfg(feature = "fixtures")]
        if let Some(hook) = &self.spawn_hook {
            return hook(terminal_id, program, cwd, env).await;
        }

        ctx.spawn_terminal(&term, &program, &cwd, &env).await
    }

    async fn plan_compensation(
        &self,
        from_phase: PhaseTag,
        reason: &str,
        output: &TxOutput,
        _op: &Operation,
    ) -> Result<CompensationStateVersioned> {
        Ok(CompensationStateVersioned {
            version: 1,
            from_phase,
            reason: reason.to_string(),
            steps: vec![CompensationStep {
                op: "rollback_terminal_card".into(),
                args: json!({
                    "card_id": output_string(output, "card_id")?,
                    "terminal_id": output_string(output, "terminal_id")?,
                    "wave_id": output_wave_id(output)?,
                }),
                completed: false,
                attempts: 0,
                last_error: None,
            }],
        })
    }

    async fn compensate_step(
        &self,
        step: &CompensationStep,
        _output: &TxOutput,
        _op: &Operation,
        ctx: &SpawnCtx,
    ) -> Result<()> {
        if step.completed {
            return Ok(());
        }
        if step.op != "rollback_terminal_card" {
            return Err(CalmError::Internal(format!(
                "unknown terminal compensation op {}",
                step.op
            )));
        }
        let card_id = step
            .args
            .get("card_id")
            .and_then(Value::as_str)
            .ok_or_else(|| CalmError::Internal("rollback step missing card_id".into()))?
            .to_string();
        let terminal_id = step
            .args
            .get("terminal_id")
            .and_then(Value::as_str)
            .ok_or_else(|| CalmError::Internal("rollback step missing terminal_id".into()))?
            .to_string();
        let wave_id = step
            .args
            .get("wave_id")
            .and_then(Value::as_str)
            .ok_or_else(|| CalmError::Internal("rollback step missing wave_id".into()))?
            .to_string();
        let card = CardId::from(card_id.clone());
        let wave = WaveId::from(wave_id);
        let scope = card_scope(ctx.repo.as_ref(), card.clone(), wave.clone()).await?;
        if let Some(term) = ctx.repo.terminal_get(&terminal_id).await? {
            reap_terminal_artifacts_with_renderer(Some(ctx.terminal_renderer.as_ref()), &term)
                .await;
        }
        let cache = self.card_role_cache.clone();
        let write = crate::state::WriteContext::new(
            self.card_role_cache.clone(),
            self.wave_cove_cache.clone(),
        );
        ctx.repo
            .write_with_event(
                ActorId::Kernel,
                scope,
                None,
                &ctx.events,
                &write,
                Box::new(move |tx| {
                    let event_card = card.clone();
                    let event_wave = wave.clone();
                    let card_id = card_id.clone();
                    let terminal_id = terminal_id.clone();
                    let cache = cache.clone();
                    Box::pin(async move {
                        crate::dispatcher::card_with_terminal_rollback_tx(
                            tx,
                            &card_id,
                            &terminal_id,
                            &cache,
                        )
                        .await?;
                        Ok(Event::CardDeleted {
                            id: event_card,
                            wave_id: event_wave,
                        })
                    })
                }),
            )
            .await?;
        Ok(())
    }
}

fn output_wave_id(output: &TxOutput) -> Result<&str> {
    output
        .result
        .get("wave_id")
        .and_then(Value::as_str)
        .ok_or_else(|| CalmError::Internal("terminal tx_output missing wave_id".into()))
}

fn output_string(output: &TxOutput, key: &str) -> Result<String> {
    output
        .data
        .get(key)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| CalmError::Internal(format!("terminal tx_output missing {key}")))
}

fn normalize_program(program: String) -> String {
    let program = program.trim();
    if program.is_empty() {
        default_program()
    } else {
        program.to_string()
    }
}

fn normalize_cwd(cwd: String) -> String {
    let cwd = cwd.trim();
    if cwd.is_empty() {
        default_cwd()
    } else {
        cwd.to_string()
    }
}

fn normalize_env(env: Value) -> Value {
    if env.is_null() { json!({}) } else { env }
}

fn default_program() -> String {
    let s = std::env::var("SHELL").unwrap_or_default();
    if s.is_empty() {
        "/bin/sh".to_string()
    } else {
        s
    }
}

fn default_cwd() -> String {
    if let Ok(home) = std::env::var("HOME")
        && !home.is_empty()
    {
        return home;
    }
    std::env::current_dir()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string()
}
