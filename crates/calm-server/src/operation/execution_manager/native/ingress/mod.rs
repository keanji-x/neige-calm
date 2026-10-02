//! Session protocol ingress. Lease phases own lifecycle; this registry only owns sockets.
mod protocol;
mod scope;

use super::super::{ExecutionManager, Record};
use super::wire::{CodexAppServer, PermissionsChoice};
use crate::error::{CalmError, Result};
use dashmap::DashMap;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use sqlx::SqlitePool;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, Notify, mpsc};
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Scope {
    execution: String,
    terminal: String,
    card: String,
    track: String,
    cwd: String,
    socket: PathBuf,
    provider: PathBuf,
    permissions: PermissionsChoice,
}
struct Gateway {
    cancel: CancellationToken,
    serial: Mutex<()>,
    stopped: Notify,
    done: std::sync::atomic::AtomicBool,
}
static GATEWAYS: OnceLock<DashMap<PathBuf, Arc<Gateway>>> = OnceLock::new();
fn gateways() -> &'static DashMap<PathBuf, Arc<Gateway>> {
    GATEWAYS.get_or_init(DashMap::new)
}

pub(super) async fn prepare(
    manager: &ExecutionManager,
    record: &Record,
    shared: Arc<super::SharedCodexAppServer>,
) -> Result<String> {
    let (provider, _) = shared.ingress_endpoints();
    let directory = provider
        .parent()
        .ok_or_else(|| CalmError::Conflict("provider socket has no directory".into()))?;
    let socket = directory.join("i").join(format!(
        "{}.sock",
        &blake3::hash(record.id.as_bytes()).to_hex()[..8]
    ));
    let scope = scope::prepare(&manager.pool, record, &socket, provider).await?;
    bind(manager.pool.clone(), scope, shared).await?;
    Ok(format!("unix://{}", socket.display()))
}

/// Rebuild only a durably frozen ingress. An unbound historical session is not upgraded.
pub(super) async fn restore(
    pool: &SqlitePool,
    execution: &str,
    shared: Arc<super::SharedCodexAppServer>,
) -> Result<String> {
    let scope = scope::load(pool, execution).await?;
    scope::require_open(pool, &scope).await?;
    bind(pool.clone(), scope.clone(), shared).await?;
    Ok(format!("unix://{}", scope.socket.display()))
}
/// Native domain admission links every producer to the same durable session group.
/// Reservation insertion, group association and the stopping fence share one tx.
pub(in crate::operation::execution_manager) async fn admit_execution_tx(
    tx: &mut crate::operation::Tx<'_>,
    owner: &super::super::Owner,
    execution: &str,
    expected: Option<&str>,
) -> Result<()> {
    scope::admit_execution_tx(tx, owner, execution, expected).await
}
pub(in crate::operation::execution_manager) async fn available_tx(
    tx: &mut crate::operation::Tx<'_>,
    owner: &super::super::Owner,
) -> Result<bool> {
    scope::available_tx(tx, owner).await
}
async fn bind(
    pool: SqlitePool,
    scope: Scope,
    shared: Arc<super::SharedCodexAppServer>,
) -> Result<()> {
    if gateways().contains_key(&scope.socket) {
        return Ok(());
    }
    let parent = scope
        .socket
        .parent()
        .ok_or_else(|| CalmError::Conflict("ingress socket has no directory".into()))?;
    tokio::fs::create_dir_all(parent).await?;
    use std::os::unix::fs::PermissionsExt;
    tokio::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).await?;
    // Never unlink a live endpoint belonging to a different server generation.
    if scope.socket.exists() {
        match UnixStream::connect(&scope.socket).await {
            Ok(_) => {
                return Err(CalmError::Conflict(
                    "native ingress endpoint belongs to another live server".into(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                tokio::fs::remove_file(&scope.socket).await?;
            }
            Err(error) => return Err(error.into()),
        }
    }
    let listener = UnixListener::bind(&scope.socket)?;
    tokio::fs::set_permissions(&scope.socket, std::fs::Permissions::from_mode(0o600)).await?;
    let gateway = Arc::new(Gateway {
        cancel: CancellationToken::new(),
        serial: Mutex::new(()),
        stopped: Notify::new(),
        done: std::sync::atomic::AtomicBool::new(false),
    });
    match gateways().entry(scope.socket.clone()) {
        dashmap::mapref::entry::Entry::Occupied(_) => {
            return Err(CalmError::Conflict(
                "native ingress bound concurrently".into(),
            ));
        }
        dashmap::mapref::entry::Entry::Vacant(entry) => {
            entry.insert(gateway.clone());
        }
    }
    tokio::spawn(async move {
        let mut clients = JoinSet::new();
        loop {
            tokio::select! {
                _ = gateway.cancel.cancelled() => break,
                accepted = listener.accept() => {
                    let Ok((stream, _)) = accepted else { break; };
                    let (pool, scope, shared, gateway) = (pool.clone(), scope.clone(), shared.clone(), gateway.clone());
                    clients.spawn(async move {
                        if let Err(error) = serve(stream, pool, scope, shared, gateway).await {
                            tracing::debug!(%error, "managed native client connection ended");
                        }
                    });
                }
                _ = clients.join_next(), if !clients.is_empty() => {}
            }
        }
        gateway.cancel.cancel();
        while clients.join_next().await.is_some() {}
        drop(listener);
        let _ = tokio::fs::remove_file(&scope.socket).await;
        gateway
            .done
            .store(true, std::sync::atomic::Ordering::Release);
        gateway.stopped.notify_waiters();
    });
    Ok(())
}

/// The caller must have committed the session stopping fence before this barrier.
pub(super) async fn quiesce(socket: &Path) -> Result<()> {
    let gateway = gateways().get(socket).map(|entry| entry.value().clone());
    if let Some(gateway) = gateway {
        gateway.cancel.cancel();
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let stopped = gateway.stopped.notified();
                if gateway.done.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                stopped.await;
            }
        })
        .await
        .map_err(|_| CalmError::Conflict("native ingress quiescence remains unconfirmed".into()))?;
        gateways().remove(socket);
    } else if socket.exists() && UnixStream::connect(socket).await.is_ok() {
        return Err(CalmError::Conflict(
            "native ingress has another live server owner".into(),
        ));
    }
    Ok(())
}

async fn serve(
    stream: UnixStream,
    pool: SqlitePool,
    scope: Scope,
    shared: Arc<super::SharedCodexAppServer>,
    gateway: Arc<Gateway>,
) -> Result<()> {
    let accepted = tokio::select! {
        _ = gateway.cancel.cancelled() => return Ok(()),
        accepted = tokio_tungstenite::accept_async(stream) => accepted,
    }
    .map_err(|error| CalmError::CodexAppServer(format!("ingress upgrade: {error}")))?;
    let (mut writer, mut reader) = accepted.split();
    let (provider, mut events) = CodexAppServer::connect_ingress(&scope.provider).await?;
    let provider = Arc::new(provider);
    let (replies, mut responses) = mpsc::channel::<Value>(32);
    let mut requests = JoinSet::new();
    let mut callbacks = std::collections::HashMap::new();
    let mut initialized = false;
    loop {
        tokio::select! {
            _ = gateway.cancel.cancelled() => break,
            reply = responses.recv() => {
                let Some(reply) = reply else { break; };
                writer.send(Message::Text(reply.to_string())).await
                    .map_err(|error| CalmError::CodexAppServer(format!("ingress response: {error}")))?;
            }
            event = events.recv() => {
                let Some(event) = event else { break; };
                if protocol::provider_event(&pool, &scope, &provider, &mut callbacks, &event).await? {
                    writer.send(Message::Text(event.to_string())).await
                        .map_err(|error| CalmError::CodexAppServer(format!("ingress event: {error}")))?;
                }
            }
            client = reader.next() => {
                let Some(Ok(message)) = client else { break; };
                if matches!(message, Message::Close(_)) { break; }
                let Message::Text(text) = message else { continue; };
                let frame: Value = serde_json::from_str(&text).map_err(|_| CalmError::Conflict("invalid native ingress JSON".into()))?;
                if frame.get("method").is_none() {
                    protocol::client_callback(&pool, &scope, &provider, &mut callbacks, frame).await?;
                    continue;
                }
                let method = frame["method"].as_str().unwrap_or("");
                if method == "initialized" && frame.get("id").is_none() && initialized {
                    provider.send_protocol_frame(frame).await?;
                    continue;
                }
                if frame.get("id").is_none() {
                    return Err(CalmError::Conflict("unsupported client notification".into()));
                }
                if method == "initialize" {
                    if initialized { return Err(CalmError::Conflict("native ingress already initialized".into())); }
                    // Initialization cannot launch execution; forward the complete capabilities.
                    let mut reply = provider.request_envelope(method, frame["params"].clone()).await?;
                    reply["id"] = frame["id"].clone();
                    initialized = reply.get("error").is_none();
                    writer.send(Message::Text(reply.to_string())).await
                        .map_err(|error| CalmError::CodexAppServer(format!("ingress initialize: {error}")))?;
                    continue;
                }
                if !initialized { return Err(CalmError::Conflict("native ingress is not initialized".into())); }
                let (pool, scope, provider, shared, gateway, replies) = (
                    pool.clone(), scope.clone(), provider.clone(), shared.clone(), gateway.clone(), replies.clone(),
                );
                if requests.len() >= 32 { return Err(CalmError::Conflict("native ingress request queue saturated".into())); }
                requests.spawn(async move {
                    let _serial = gateway.serial.lock().await;
                    let id = frame["id"].clone();
                    let handled = tokio::select! {
                        _ = gateway.cancel.cancelled() => return,
                        result = protocol::request(&pool, &scope, &provider, &shared, frame) => result,
                    };
                    let reply = match handled {
                        Ok(mut reply) => { reply["id"] = id; reply }
                        Err(error) => json!({"jsonrpc":"2.0","id":id,"error":{"code":-32000,"message":error.to_string()}}),
                    };
                    let _ = replies.send(reply).await;
                });
            }
            _ = requests.join_next(), if !requests.is_empty() => {}
        }
    }
    requests.abort_all();
    while requests.join_next().await.is_some() {}
    Ok(())
}

#[cfg(test)]
mod tests;
