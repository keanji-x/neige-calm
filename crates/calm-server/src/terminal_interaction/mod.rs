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
use client::Client;
pub(crate) use target::Binding;
pub use target::Target;
#[cfg(test)]
pub(crate) use target::TaskBinding;

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
    pub async fn observe(
        &self,
        identity: &ToolCallIdentity,
        target: &Target,
        offset: usize,
        wait_ms: u64,
        format: ObservationFormat,
    ) -> Result<(Value, Option<Vec<u8>>)> {
        ensure!(wait_ms <= 2000, "observation wait exceeds 2000ms");
        let resolved = Self::resolve_target(self.repo.as_ref(), identity, target).await?;
        let client = self.client(identity, &resolved.binding).await?;
        self.capture(identity, resolved, &client, offset, wait_ms, format)
            .await
    }
    async fn capture(
        &self,
        identity: &ToolCallIdentity,
        resolved: target::Resolved,
        client: &Client,
        offset: usize,
        wait_ms: u64,
        format: ObservationFormat,
    ) -> Result<(Value, Option<Vec<u8>>)> {
        let terminal = resolved.binding.terminal_id.as_str();
        if wait_ms > 0 {
            tokio::time::sleep(Duration::from_millis(wait_ms)).await;
        }
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
        Self::check_binding(self.repo.as_ref(), identity, &resolved.binding, false).await?;
        let observation_id = Uuid::new_v4();
        let mut metadata = json!({"terminal_id":terminal,"observation_id":observation_id,"connection_id":client.connection,
            "terminal_session_id":client.entry.handle.session_id,"control_id":control,"role":if control.is_some(){"owner"}else{"observer"},
            "task_status":resolved.task_status,"controllable":resolved.controllable,"task":resolved.binding.task,"worker_session_id":resolved.binding.worker_session_id,"card_id":resolved.binding.card_id,
            "observation_revision":revision.to_string(),"cols":frame.cols,"rows":frame.rows,"cursor":frame.cursor,
            "alternate":frame.alternate,"scroll_offset":frame.scroll_offset,"history_rows":frame.history_rows,
            "text":frame.text,"exited":exited});
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
        Ok((metadata, png))
    }
    pub async fn control(
        &self,
        identity: &ToolCallIdentity,
        target: &Target,
        action: &str,
        observation_wait_ms: Option<u64>,
    ) -> Result<Value> {
        ensure!(
            observation_wait_ms.is_none_or(|wait| wait <= 2000),
            "observation wait exceeds 2000ms"
        );
        ensure!(
            action != "detach" || observation_wait_ms.is_none(),
            "detach cannot request observation"
        );
        if action == "detach" {
            Self::authorize(self.repo.as_ref(), identity).await?;
            self.clients.lock().await.retain(|key, client| {
                let selected = match target {
                    Target::Terminal(id) => &client.binding.terminal_id == id,
                    Target::Task(id) => client
                        .binding
                        .task
                        .as_ref()
                        .is_some_and(|task| &task.task_id == id),
                };
                !(selected && key == &client.binding.key(identity))
            });
            return Ok(json!({"detached":true}));
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
        Ok(self
            .with_observation(identity, &client, receipt, observation_wait_ms)
            .await)
    }
}
