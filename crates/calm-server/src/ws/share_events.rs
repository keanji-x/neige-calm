//! Live invalidation for one share scope. No replay cursor or Event payload crosses
//! this boundary: a client re-reads the finite resource owners after invalidation.
use crate::event::BroadcastEnvelope;
use crate::extract::Path;
use crate::{
    auth::AuthState,
    error::{ErrorBody, Result},
    state::{AppState, RouteState},
};
use axum::{
    Extension, Router,
    extract::{
        State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    response::Response,
    routing::get,
};
use calm_types::{event::EventScope, share_links::ShareScope};
use futures_util::SinkExt;
use serde::Serialize;
use tokio::sync::{broadcast, watch};

pub fn router() -> Router<AppState> {
    Router::new().route("/api/share/{token}/events", get(upgrade))
}

#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Frame<'a> {
    Snapshot { scope: &'a ShareScope },
    Invalidate { scope: &'a ShareScope },
    Closed,
}

#[utoipa::path(get, path="/api/share/{token}/events", tag="share_links", security(()),
    params(("token"=String,Path,description="Share access token")),
    responses(
        (status=101,description="Scope-only invalidation WebSocket"),
        (status=403,description="Share authority no longer current",body=ErrorBody),
        (status=404,description="Active share not found",body=ErrorBody),
        (status=500,description="Internal error",body=ErrorBody)))]
pub(crate) async fn upgrade(
    State(s): State<RouteState>,
    Extension(auth): Extension<AuthState>,
    Path(token): Path<String>,
    ws: WebSocketUpgrade,
) -> Result<Response> {
    // Both subscriptions precede every awaited access/snapshot read. Changes
    // during the handshake remain buffered and force the next current decision.
    let events = s.events.subscribe();
    let wake = s.events.subscribe_access_changes();
    let access = crate::share_access::resolve(s.repo.as_ref(), &auth.config, &token, None).await?;
    Ok(ws.on_upgrade(move |socket| handle(socket, s, auth, token, access, events, wake)))
}

async fn send(socket: &mut WebSocket, frame: Frame<'_>) -> bool {
    let Ok(value) = serde_json::to_string(&frame) else {
        return false;
    };
    socket.send(Message::Text(value.into())).await.is_ok()
}

async fn handle(
    mut socket: WebSocket,
    s: RouteState,
    auth: AuthState,
    token: String,
    access: crate::share_access::ShareRead,
    mut events: broadcast::Receiver<BroadcastEnvelope>,
    mut wake: watch::Receiver<()>,
) {
    let scope = &access.link.delegation.scope;
    #[cfg(feature = "fixtures")]
    if let ShareScope::Track(id) = scope
        && crate::track_access::after_assembly(id).await.is_err()
    {
        return;
    }
    if crate::share_access::require_publication(
        s.repo.as_ref(),
        &auth.config,
        &token,
        None,
        &access,
    )
    .await
    .is_err()
    {
        let _ = send(&mut socket, Frame::Closed).await;
        let _ = socket.close().await;
        return;
    }
    if !send(&mut socket, Frame::Snapshot { scope }).await {
        return;
    }
    loop {
        let (event_scope, invalidate) = tokio::select! {
            change = wake.changed() => {
                if change.is_err() { break; }
                (None, false)
            },
            event = events.recv() => match event {
                Ok(envelope) => (Some(envelope.scope), true),
                Err(broadcast::error::RecvError::Lagged(_)) => (None, true),
                Err(broadcast::error::RecvError::Closed) => break,
            },
            incoming = socket.recv() => match incoming {
                Some(Ok(Message::Ping(bytes))) => {
                    if socket.send(Message::Pong(bytes)).await.is_err() { break; }
                    continue;
                }
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => break,
                _ => continue,
            },
        };
        let matches = match (scope, event_scope.as_ref()) {
            (_, None) => true, // lag: re-fetch this scope only
            (ShareScope::Track(id), Some(event)) => {
                event.track_id().is_some_and(|track| track.as_str() == id)
            }
            (ShareScope::Area(id), Some(event)) => {
                if event.area_id().is_none_or(|area| area.as_str() != id) {
                    false
                } else if let Some(track) = event.track_id() {
                    // Dynamic containment also excludes retired Area chat rows.
                    match crate::share_access::resolve(
                        s.repo.as_ref(),
                        &auth.config,
                        &token,
                        Some(track.as_str()),
                    )
                    .await
                    {
                        Ok(_) => true,
                        // A stale event scope still invalidates the Area list after
                        // deletion/move. It reveals no entity identity or payload.
                        Err(crate::error::CalmError::NotFound(_)) => {
                            match s.repo.track_get(track.as_str()).await {
                                Ok(None) => true,
                                Ok(Some(row)) => {
                                    row.area_id.as_str() != id
                                        && row.purpose.as_deref() != Some(crate::AREA_CHAT_PURPOSE)
                                }
                                Err(_) => false,
                            }
                        }
                        Err(_) => false,
                    }
                } else {
                    matches!(event, EventScope::Area { .. })
                }
            }
        };
        // Recheck even on unrelated events: deletion/ownership changes must end
        // the lease. Wake notifications are access-only, not invalidation data.
        if crate::share_access::require_publication(
            s.repo.as_ref(),
            &auth.config,
            &token,
            None,
            &access,
        )
        .await
        .is_err()
        {
            let _ = send(&mut socket, Frame::Closed).await;
            break;
        }
        if invalidate && matches && !send(&mut socket, Frame::Invalidate { scope }).await {
            break;
        }
    }
    let _ = socket.close().await;
}
