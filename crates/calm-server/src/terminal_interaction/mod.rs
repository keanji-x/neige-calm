//! Planner-owned clients of the same renderer and PTY as human Terminal cards.
use crate::db::RouteRepo;
use crate::mcp_server::registry::ToolCallIdentity;
use crate::model::CardRole;
use crate::terminal_renderer::{ClientInputScope, TerminalRendererRegistry};
use anyhow::{Result, ensure};
use calm_session::ClientMsg;
use calm_terminal_view::{InputSurface, Rasterizer};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OnceCell};
use uuid::Uuid;

mod action_observation;
mod client;
mod observation;
mod operations;
pub use observation::ObservationFormat;
mod target;
mod wait;
use client::{Client, LatestObservation};
pub(crate) use target::Binding;
pub use target::Target;
#[cfg(test)]
pub(crate) use target::TaskBinding;
pub use wait::{
    SETTLE_MS_DEFAULT, SETTLE_MS_MAX, SIGNAL_WAIT_MS_DEFAULT, WAIT_MS_MAX, WaitFor, WaitPlan,
};

/// Baseline an action readback compares against: the projection revision and
/// the signal seq read immediately before the physical action (#1618/#1620).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ReadbackBaseline {
    pub revision: u64,
    pub signal_seq: u64,
}

/// Signals listed on one observation (the most recent ones since the
/// previous observation on the connection; the rest are counted as dropped).
pub const SIGNALS_PER_OBSERVATION: usize = 20;

pub struct TerminalInteraction {
    repo: Arc<dyn RouteRepo>,
    renderer: Arc<TerminalRendererRegistry>,
    clients: Mutex<HashMap<String, Arc<Client>>>,
    raster: OnceCell<Arc<Rasterizer>>,
    observations: StdMutex<HashMap<Uuid, Observation>>,
}
struct Observation {
    binding: String,
    connection: Uuid,
    revision: u64,
    control: Option<Uuid>,
    surface: InputSurface,
    created: Instant,
}
impl TerminalInteraction {
    pub fn new(repo: Arc<dyn RouteRepo>, renderer: Arc<TerminalRendererRegistry>) -> Self {
        Self {
            repo,
            renderer,
            clients: Mutex::new(HashMap::new()),
            raster: OnceCell::new(),
            observations: StdMutex::new(HashMap::new()),
        }
    }
    pub async fn authorize(repo: &dyn RouteRepo, identity: &ToolCallIdentity) -> Result<String> {
        ensure!(
            identity.role == CardRole::Planner,
            "planner-only terminal tool"
        );
        let session = repo
            .session_get_by_id(&identity.session_id.clone().into())
            .await?
            .ok_or_else(|| anyhow::anyhow!("planner session unavailable"))?;
        ensure!(
            session.state.is_active_authority(),
            "planner session authority ended"
        );
        let current = repo
            .card_identity_get_by_session(&identity.session_id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("planner card unavailable"))?;
        ensure!(
            current.role == CardRole::Planner
                && current.card_id.as_str() == identity.card_id
                && Some(current.track_id.as_str()) == identity.track_id.as_deref()
                && current.area_id.as_str() == identity.area_id,
            "planner identity or Track changed"
        );
        Ok(current.track_id.to_string())
    }
    async fn client(&self, identity: &ToolCallIdentity, resolved: &Binding) -> Result<Arc<Client>> {
        Self::check_binding(self.repo.as_ref(), identity, resolved, false).await?;
        let terminal = resolved.terminal_id.as_str();
        let entry = self.renderer.get(terminal).ok_or_else(|| {
            anyhow::anyhow!("terminal unavailable; observation never starts a process")
        })?;
        let binding = resolved.key(identity);
        let mut clients = self.clients.lock().await;
        clients.retain(|_, client| {
            client.screen.lock().is_ok_and(|state| state.available)
                && self
                    .renderer
                    .get(&client.entry.terminal_id)
                    .is_some_and(|entry| Arc::ptr_eq(&entry, &client.entry))
        });
        if let Some(client) = clients.get(&binding) {
            *client.last_used.lock().unwrap() = Instant::now();
            ensure!(
                Arc::ptr_eq(&client.entry, &entry),
                "terminal generation changed; open a new terminal"
            );
            return Ok(client.clone());
        }
        ensure!(clients.len() < 128, "Planner terminal client limit reached");
        let scope = Self::bound_scope(self.repo.clone(), identity, resolved);
        let client = Arc::new(Client::attach(entry, scope, resolved.clone()).await?);
        clients.insert(binding, client.clone());
        Ok(client)
    }
    pub(crate) fn bound_scope(
        repo: Arc<dyn RouteRepo>,
        identity: &ToolCallIdentity,
        resolved: &Binding,
    ) -> ClientInputScope {
        let scope_check = |write: bool| {
            let repo = repo.clone();
            let actor = identity.clone();
            let expected = resolved.clone();
            Arc::new(move || {
                let repo = repo.clone();
                let actor = actor.clone();
                let expected = expected.clone();
                Box::pin(async move {
                    Self::check_binding(repo.as_ref(), &actor, &expected, write)
                        .await
                        .is_ok()
                }) as futures::future::BoxFuture<'static, bool>
            })
                as Arc<dyn Fn() -> futures::future::BoxFuture<'static, bool> + Send + Sync>
        };
        ClientInputScope::Bound {
            observe: scope_check(false),
            control: scope_check(true),
        }
    }
    /// Whether a Planner input on `terminal_id` is reserved but not yet
    /// acknowledged or refused (the write is parked at the physical barrier
    /// or in flight). Test observability only; no tool reports it.
    #[doc(hidden)]
    pub async fn input_pending(&self, terminal_id: &str) -> bool {
        self.clients.lock().await.values().any(|client| {
            client.binding.terminal_id == terminal_id
                && client
                    .screen
                    .lock()
                    .is_ok_and(|state| state.pending.is_some())
        })
    }
    /// Inputs on `terminal_id` waiting for their connection's serial lock
    /// (queued behind an action still in progress). Test observability only;
    /// no tool reports it.
    #[doc(hidden)]
    pub async fn serial_waiters(&self, terminal_id: &str) -> usize {
        self.clients
            .lock()
            .await
            .values()
            .filter(|client| client.binding.terminal_id == terminal_id)
            .map(|client| client.serial_waiters())
            .sum()
    }
    pub async fn observe(
        &self,
        identity: &ToolCallIdentity,
        target: &Target,
        offset: usize,
        wait: WaitPlan,
        format: ObservationFormat,
    ) -> Result<(Value, Option<Vec<u8>>)> {
        wait.validate()?;
        let resolved = Self::resolve_target(self.repo.as_ref(), identity, target).await?;
        let client = self.client(identity, &resolved.binding).await?;
        self.capture(identity, resolved, &client, offset, wait, None, format)
            .await
    }
    /// `baseline` is the revision (and signal seq) a change or signal wait
    /// compares against; `None` means this connection's previous observation
    /// (or the state at call start when there is none).
    #[allow(clippy::too_many_arguments)]
    async fn capture(
        &self,
        identity: &ToolCallIdentity,
        resolved: target::Resolved,
        client: &Client,
        offset: usize,
        wait: WaitPlan,
        baseline: Option<ReadbackBaseline>,
        format: ObservationFormat,
    ) -> Result<(Value, Option<Vec<u8>>)> {
        let terminal = resolved.binding.terminal_id.clone();
        let previous = *client
            .latest_observation
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal client poisoned"))?;
        let (baseline, signal_baseline) = match (baseline, previous) {
            (Some(baseline), _) => (baseline.revision, baseline.signal_seq),
            (None, Some(previous)) => (previous.revision, previous.last_seq),
            (None, None) => (
                client
                    .entry
                    .handle
                    .model_view
                    .lock()
                    .map_err(|_| anyhow::anyhow!("terminal view poisoned"))?
                    .capture(0)?
                    .1,
                client.entry.signals.last_seq(),
            ),
        };
        let waited = wait::wait(client, &wait, baseline, signal_baseline).await;
        // The wait may span a task completion or an authority change, so the
        // task status and controllability in the result are re-read after
        // waiting; the binding they belong to must still be the one the wait
        // started on, otherwise the observation is refused.
        let resolved =
            Self::check_binding(self.repo.as_ref(), identity, &resolved.binding, false).await?;
        let (control, exited) = {
            let state = client
                .screen
                .lock()
                .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
            ensure!(state.available, "terminal observation disconnected");
            (state.control, state.exited)
        };
        let (frame, revision) = client
            .entry
            .handle
            .model_view
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal view poisoned"))?
            .capture(offset)?;
        let png = format.render_image(&self.raster, &frame).await?;
        // Rendering takes time too: the emitted status is the last read.
        let resolved =
            Self::check_binding(self.repo.as_ref(), identity, &resolved.binding, false).await?;
        let observation_id = Uuid::new_v4();
        let changed_since_previous = previous.is_some_and(|prior| prior.revision != revision);
        // #1620 — the listed signals and the recorded `last_seq` come from one
        // ring read, so advancing this connection's baseline to `last_seq`
        // cannot skip a signal that was never listed. Untrusted telemetry:
        // presentation only, no fence reads it.
        let signals = client.entry.signals.since(
            previous
                .map(|prior| prior.last_seq)
                .unwrap_or(signal_baseline),
            SIGNALS_PER_OBSERVATION,
        );
        let mut metadata = json!({"terminal_id":terminal,"observation_id":observation_id,"connection_id":client.connection,
            "terminal_session_id":client.entry.handle.session_id,"control_id":control,"role":if control.is_some(){"owner"}else{"observer"},
            "task_status":resolved.task_status,"controllable":resolved.controllable,"task":resolved.binding.task,"worker_session_id":resolved.binding.worker_session_id,"card_id":resolved.binding.card_id,
            "observation_revision":revision.to_string(),"cols":frame.cols,"rows":frame.rows,"cursor":frame.cursor,
            "alternate":frame.alternate,"scroll_offset":frame.scroll_offset,"history_rows":frame.history_rows,
            "text":frame.text,"exited":exited,"wait":waited.to_json(),"changed_since_previous_observation":changed_since_previous,
            "previous_observation_revision":previous.map(|prior| prior.revision.to_string()),
            "signals":{"hooks_seen":signals.last_seq > 0,"last_seq":signals.last_seq,
                "since_previous_observation":signals.signals.iter().map(|signal| signal.to_json()).collect::<Vec<_>>(),
                "dropped_since_previous_observation":signals.dropped}});
        if png.is_some() {
            metadata["image_source"] = json!("rmux_client_projection");
        }
        let mut observations = self
            .observations
            .lock()
            .map_err(|_| anyhow::anyhow!("observation registry poisoned"))?;
        observations.retain(|_, value| value.created.elapsed() < Duration::from_secs(120));
        ensure!(
            observations.len() < 1024,
            "terminal observation limit reached"
        );
        observations.insert(
            observation_id,
            Observation {
                binding: resolved.binding.key(identity),
                connection: client.connection,
                revision,
                control,
                surface: frame.input_surface(),
                created: Instant::now(),
            },
        );
        *client
            .latest_observation
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal client poisoned"))? = Some(LatestObservation {
            id: observation_id,
            revision,
            scroll_offset: frame.scroll_offset,
            last_seq: signals.last_seq,
        });
        Ok((metadata, png))
    }
    /// #1620 `open claim:true`: claim control right after creation and return
    /// the claim receipt with its readback. Unlike an explicit
    /// `control claim`, an open never revokes a holder: on a fresh terminal
    /// nobody holds control, so this only matters for a replayed open (same
    /// request_id) after a human takeover, which is reported as unavailable
    /// instead of being taken back silently. Control already held by this
    /// connection returns the current observation without a second claim.
    pub async fn claim_after_open(
        &self,
        identity: &ToolCallIdentity,
        target: &Target,
        readback: WaitPlan,
    ) -> Result<Value> {
        readback.validate()?;
        let resolved = Self::resolve_target(self.repo.as_ref(), identity, target).await?;
        let client = self.client(identity, &resolved.binding).await?;
        let (owner, control) = {
            let state = client
                .screen
                .lock()
                .map_err(|_| anyhow::anyhow!("terminal state poisoned"))?;
            (state.owner, state.control)
        };
        if control.is_some() {
            let receipt = json!({"terminal_id":resolved.binding.terminal_id,"connection_id":client.connection,"control_id":control});
            return Ok(self
                .with_observation(identity, &client, receipt, Some(readback), None)
                .await);
        }
        ensure!(
            owner.is_none(),
            "terminal control is held by another client since this terminal was opened (human takeover); open does not reclaim it, claim deliberately with calm.terminal.control"
        );
        drop(client);
        self.control(identity, target, "claim", Some(readback))
            .await
    }
    pub async fn control(
        &self,
        identity: &ToolCallIdentity,
        target: &Target,
        action: &str,
        observation_wait: Option<WaitPlan>,
    ) -> Result<Value> {
        if let Some(wait) = &observation_wait {
            wait.validate()?;
        }
        ensure!(
            action != "detach" || observation_wait.is_none(),
            "detach cannot request observation"
        );
        if action == "detach" {
            Self::authorize(self.repo.as_ref(), identity).await?;
            let mut removed = Vec::new();
            self.clients.lock().await.retain(|key, client| {
                let selected = match target {
                    Target::Terminal(id) => &client.binding.terminal_id == id,
                    Target::Task(id) => client
                        .binding
                        .task
                        .as_ref()
                        .is_some_and(|task| &task.task_id == id),
                };
                let detach = selected && key == &client.binding.key(identity);
                if detach {
                    removed.push(client.clone());
                }
                !detach
            });
            let closed = removed.into_iter().next();
            let terminal_id = match &closed {
                Some(client) => Some(client.binding.terminal_id.clone()),
                None => Self::resolve_target(self.repo.as_ref(), identity, target)
                    .await
                    .ok()
                    .map(|resolved| resolved.binding.terminal_id),
            };
            return Ok(
                json!({"detached":true,"had_client":closed.is_some(),"terminal_id":terminal_id,
                "connection_id":closed.as_ref().map(|client| client.connection),
                "terminal_session_id":closed.as_ref().map(|client| client.entry.handle.session_id)}),
            );
        }
        let resolved = Self::resolve_target(self.repo.as_ref(), identity, target).await?;
        let terminal = resolved.binding.terminal_id.as_str();
        Self::check_binding(
            self.repo.as_ref(),
            identity,
            &resolved.binding,
            action == "claim",
        )
        .await?;
        let client = self.client(identity, &resolved.binding).await?;
        let _serial = client.serial.lock().await;
        // Readback change/signal waits compare against the screen and the
        // signal seq as they were when the control action started.
        let signal_seq = client.entry.signals.last_seq();
        let baseline = client
            .entry
            .handle
            .model_view
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal view poisoned"))?
            .capture(0)
            .map(|(_, revision)| ReadbackBaseline {
                revision,
                signal_seq,
            })
            .ok();
        match action {
            "claim" => {
                let previous = client.screen.lock().unwrap().control;
                client.send(ClientMsg::OwnerClaim).await?;
                client
                    .wait(
                        |state| state.owner == Some(client.id) && state.control != previous,
                        Duration::from_secs(7),
                    )
                    .await?;
            }
            "release" => {
                client.send(ClientMsg::OwnerRelease).await?;
                client
                    .wait(|state| state.control.is_none(), Duration::from_secs(7))
                    .await?;
            }
            _ => anyhow::bail!("unknown terminal control action"),
        }
        let receipt = {
            let state = client.screen.lock().unwrap();
            json!({"terminal_id":terminal,"connection_id":client.connection,"control_id":state.control})
        };
        // Read before the readback registers its own capture as the latest.
        let previous = *client
            .latest_observation
            .lock()
            .map_err(|_| anyhow::anyhow!("terminal client poisoned"))?;
        let mut receipt = self
            .with_observation(identity, &client, receipt, observation_wait, baseline)
            .await;
        if action == "release" {
            action_observation::omit_unchanged_release_text(&mut receipt, previous);
        }
        Ok(receipt)
    }
}
/// The input-surface fence: size, input modes and alternate screen. The
/// scroll offset is a separate fence (live viewport only).
pub(crate) fn same_input_surface(saved: &InputSurface, live: &InputSurface) -> bool {
    saved.cols == live.cols
        && saved.rows == live.rows
        && saved.modes == live.modes
        && saved.alternate == live.alternate
}
