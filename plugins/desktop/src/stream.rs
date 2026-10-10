//! The `http_socket` server: one route, `GET /apps/{app}/stream`, a WebSocket
//! that speaks window-stream protocol v1 for the app's main window. The
//! kernel's session- and origin-gated route is the only way a browser reaches
//! it (A2 is the kernel's); this side checks nothing about the caller.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::ws::{
    Message, WebSocket, WebSocketUpgrade, rejection::WebSocketUpgradeRejection,
};
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Router, routing::get};
use futures_util::{Sink, Stream};
use tokio::net::UnixListener;
use tokio_util::sync::CancellationToken;
use window_stream::{DEFAULT_JPEG_QUALITY, JpegEncoder, WindowSource, WsMessage, serve_viewer};

use crate::Desktop;
use crate::config::CHROME;
use crate::feed::{CompositorWindow, NoWindow};

/// How long a new viewer waits for the app's main window before `closed`.
pub const MAIN_WINDOW_WAIT: Duration = Duration::from_secs(30);

pub fn router(desktop: Arc<Desktop>) -> Router {
    Router::new()
        .route("/apps/{app}/stream", get(stream))
        .with_state(desktop)
}

/// Serves `listener` until `shutdown`.
pub async fn serve(listener: UnixListener, desktop: Arc<Desktop>, shutdown: CancellationToken) {
    let app = router(desktop);
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async move { shutdown.cancelled().await })
        .await;
    if let Err(e) = result {
        tracing::warn!(error = %e, "the http socket server failed");
    }
}

async fn stream(
    State(desktop): State<Arc<Desktop>>,
    Path(app): Path<String>,
    upgrade: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Response {
    if app != CHROME {
        return (StatusCode::NOT_FOUND, format!("no app `{app}`")).into_response();
    }
    match upgrade {
        Err(rejection) => rejection.into_response(),
        Ok(upgrade) => upgrade.on_upgrade(move |socket| async move {
            let source = main_window_source(&desktop).await;
            let summary = serve_viewer(
                AxumSocket(socket),
                source,
                Box::new(JpegEncoder::new(DEFAULT_JPEG_QUALITY)),
            )
            .await;
            tracing::debug!(?summary, "viewer session ended");
        }),
    }
}

/// Launches the app if it is not running and waits for its main window. A
/// running app is served from its published state, never behind the app's
/// queue, where an agent's navigation may be waiting on a hung page.
async fn main_window_source(desktop: &Desktop) -> Arc<dyn WindowSource> {
    let running = desktop.app.state().pid;
    let pid = match running {
        Some(pid) => pid,
        None => match desktop.app.start().await {
            Ok(pid) => pid,
            Err(e) => return Arc::new(NoWindow(e.to_string())),
        },
    };
    let state = desktop.app.subscribe();
    let still_running = move || state.borrow().pid == Some(pid);
    match desktop
        .windows
        .wait_main_window(pid, MAIN_WINDOW_WAIT, still_running)
        .await
    {
        Some(window) => Arc::new(CompositorWindow::main_window(
            desktop.compositor.clone(),
            desktop.windows.clone(),
            window.id,
            pid,
        )),
        None => Arc::new(NoWindow(format!(
            "Chrome (pid {pid}) showed no window within {MAIN_WINDOW_WAIT:?}"
        ))),
    }
}

/// axum's WebSocket as `window-stream`'s message stream and sink. Readiness
/// passes through unchanged and nothing is buffered here, so the session's
/// latest-wins pacing sees the socket itself. Ping and pong are axum's.
struct AxumSocket(WebSocket);

impl Stream for AxumSocket {
    type Item = Result<WsMessage, axum::Error>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            let message = match Pin::new(&mut self.0).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e))),
                Poll::Ready(Some(Ok(message))) => message,
            };
            return Poll::Ready(Some(Ok(match message {
                Message::Text(text) => WsMessage::Text(text.to_string()),
                Message::Binary(bytes) => WsMessage::Binary(bytes.to_vec()),
                Message::Close(_) => WsMessage::Close,
                Message::Ping(_) | Message::Pong(_) => continue,
            })));
        }
    }
}

impl Sink<WsMessage> for AxumSocket {
    type Error = axum::Error;

    fn poll_ready(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0).poll_ready(cx)
    }

    fn start_send(mut self: Pin<&mut Self>, item: WsMessage) -> Result<(), Self::Error> {
        let message = match item {
            WsMessage::Text(text) => Message::Text(text.into()),
            WsMessage::Binary(bytes) => Message::Binary(bytes.into()),
            WsMessage::Close => Message::Close(None),
        };
        Pin::new(&mut self.0).start_send(message)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_close(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Pin::new(&mut self.0).poll_close(cx)
    }
}
