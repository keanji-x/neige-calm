//! Planner-owned clients of the same renderer and PTY as human Terminal cards.
use crate::db::RouteRepo;
use crate::mcp_server::registry::ToolCallIdentity;
use crate::model::CardRole;
use crate::terminal_renderer::{ClientInputScope, TerminalRendererRegistry};
use anyhow::{Result, ensure};
use calm_session::ClientMsg;
use calm_terminal_view::{Frame, Rasterizer};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, OnceCell};
use uuid::Uuid;

mod client;
mod operations;
use client::Client;

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
    frame: Frame,
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
    fn binding(identity: &ToolCallIdentity, terminal: &str) -> String {
        format!("{}:{terminal}", identity.session_id)
    }
    pub async fn authorize(
        repo: &dyn RouteRepo,
        identity: &ToolCallIdentity,
        terminal: Option<&str>,
    ) -> Result<String> {
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
        if let Some(terminal) = terminal {
            let term = repo
                .terminal_get(terminal)
                .await?
                .ok_or_else(|| anyhow::anyhow!("terminal unavailable"))?;
            let card = repo
                .card_get(term.card_id.as_str())
                .await?
                .ok_or_else(|| anyhow::anyhow!("terminal card unavailable"))?;
            ensure!(
                card.kind == "terminal" && card.track_id == current.track_id,
                "terminal outside Planner Track"
            );
        }
        Ok(current.track_id.to_string())
    }
    async fn client(&self, identity: &ToolCallIdentity, terminal: &str) -> Result<Arc<Client>> {
        Self::authorize(self.repo.as_ref(), identity, Some(terminal)).await?;
        let entry = self.renderer.get(terminal).ok_or_else(|| {
            anyhow::anyhow!("terminal unavailable; observation never starts a process")
        })?;
        let binding = Self::binding(identity, terminal);
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
        let repo = self.repo.clone();
        let actor = identity.clone();
        let terminal = terminal.to_owned();
        let scope = ClientInputScope::Bound(Arc::new(move || {
            let repo = repo.clone();
            let actor = actor.clone();
            let terminal = terminal.clone();
            Box::pin(async move {
                Self::authorize(repo.as_ref(), &actor, Some(&terminal))
                    .await
                    .is_ok()
            })
        }));
        let client = Arc::new(Client::attach(entry, scope).await?);
        clients.insert(binding, client.clone());
        Ok(client)
    }
    pub async fn observe(
        &self,
        identity: &ToolCallIdentity,
        terminal: &str,
        offset: usize,
        wait_ms: u64,
    ) -> Result<(Value, Vec<u8>)> {
        ensure!(wait_ms <= 2000, "observation wait exceeds 2000ms");
        let client = self.client(identity, terminal).await?;
        if wait_ms > 0 {
            tokio::time::sleep(Duration::from_millis(wait_ms)).await;
        }
        Self::authorize(self.repo.as_ref(), identity, Some(terminal)).await?;
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
        let raster = self
            .raster
            .get_or_try_init(|| async {
                tokio::task::spawn_blocking(Rasterizer::system)
                    .await?
                    .map(Arc::new)
            })
            .await?
            .clone();
        let image_frame = frame.clone();
        let png = tokio::task::spawn_blocking(move || raster.png(&image_frame)).await??;
        Self::authorize(self.repo.as_ref(), identity, Some(terminal)).await?;
        let observation_id = Uuid::new_v4();
        let metadata = json!({"terminal_id":terminal,"observation_id":observation_id,"connection_id":client.connection,
            "terminal_session_id":client.entry.handle.session_id,"control_id":control,"role":if control.is_some(){"owner"}else{"observer"},
            "observation_revision":revision.to_string(),"cols":frame.cols,"rows":frame.rows,"cursor":frame.cursor,
            "alternate":frame.alternate,"scroll_offset":frame.scroll_offset,"history_rows":frame.history_rows,
            "text":frame.text,"exited":exited,"image_source":"rmux_client_projection"});
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
                binding: Self::binding(identity, terminal),
                connection: client.connection,
                revision,
                control,
                frame,
                created: Instant::now(),
            },
        );
        Ok((metadata, png))
    }
    pub async fn control(
        &self,
        identity: &ToolCallIdentity,
        terminal: &str,
        action: &str,
    ) -> Result<Value> {
        if action == "detach" {
            Self::authorize(self.repo.as_ref(), identity, None).await?;
            let removed = self
                .clients
                .lock()
                .await
                .remove(&Self::binding(identity, terminal));
            drop(removed);
            return Ok(json!({"detached":true}));
        }
        let client = self.client(identity, terminal).await?;
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
        let state = client.screen.lock().unwrap();
        Ok(
            json!({"terminal_id":terminal,"connection_id":client.connection,"control_id":state.control}),
        )
    }
}
