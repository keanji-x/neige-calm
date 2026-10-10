//! One listener per pool port; every request is reverse-proxied to that port's registered
//! `127.0.0.1` target through [`crate::reverse_proxy`]. Bodies stream both ways; an upgrade (vite
//! HMR) becomes a byte tunnel.
//!
//! The pool is reachable from the LAN and `Host` is rewritten to loopback (which defeats a dev
//! server's own DNS-rebinding checks), so every request must carry calm's session first; an
//! unauthenticated request never reaches the target. CORS preflights carry no credentials, so
//! they get 401 too: a preview calling another preview port must go through its dev server's
//! proxy. An open tunnel outlives logout, as calm's own WS routes do.

use super::PreviewRegistry;
use crate::auth::{AuthState, resolve_principal};
use crate::reverse_proxy::{self, Target, Upstream};
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{Html, IntoResponse, Response};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

/// Wait for response headers; generous because vite's first on-demand compile can be slow.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone)]
struct Gateway {
    registry: Arc<PreviewRegistry>,
    auth: AuthState,
    port: u16,
    shutdown: CancellationToken,
    response_timeout: Duration,
}

/// Binds every pool port on `host` (a bind failure is a boot error) and serves until `shutdown`.
pub async fn spawn(
    registry: Arc<PreviewRegistry>,
    auth: AuthState,
    host: &str,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    for &port in registry.pool() {
        let listener = TcpListener::bind((host, port))
            .await
            .map_err(|e| anyhow::anyhow!("preview gateway bind {host}:{port}: {e}"))?;
        serve(listener, registry.clone(), auth.clone(), shutdown.clone())?;
    }
    if !registry.pool().is_empty() {
        tracing::info!(host, ports = ?registry.pool(), "preview gateway listening");
    }
    Ok(())
}

/// Serves one already-bound pool port; the listener's port is the pool port it answers for.
pub fn serve(
    listener: TcpListener,
    registry: Arc<PreviewRegistry>,
    auth: AuthState,
    shutdown: CancellationToken,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    serve_with_response_timeout(listener, registry, auth, shutdown, RESPONSE_TIMEOUT)
}

/// [`serve`] with an explicit response-header timeout.
pub fn serve_with_response_timeout(
    listener: TcpListener,
    registry: Arc<PreviewRegistry>,
    auth: AuthState,
    shutdown: CancellationToken,
    response_timeout: Duration,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    let port = listener.local_addr()?.port();
    let gateway = Gateway {
        registry,
        auth,
        port,
        shutdown: shutdown.clone(),
        response_timeout,
    };
    let app = axum::Router::new().fallback(proxy).with_state(gateway);
    Ok(tokio::spawn(async move {
        let served = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown.cancelled_owned())
        .await;
        if let Err(error) = served {
            tracing::warn!(port, %error, "preview gateway listener stopped");
        }
    }))
}

async fn proxy(
    State(gw): State<Gateway>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Response {
    if resolve_principal(&gw.auth, req.headers()).is_none() {
        return page(
            StatusCode::UNAUTHORIZED,
            "Preview locked",
            "Log in to calm on this host first, then reload.",
        );
    }
    let Some(entry) = gw.registry.lookup(gw.port) else {
        let body = format!("no preview is registered on port {}\n", gw.port);
        return (StatusCode::NOT_FOUND, body).into_response();
    };
    let target = entry.target_port;
    let mut own_host = None;
    // Tied to the registration and the gateway: unregister, re-target or shutdown closes a tunnel.
    let closes_on = vec![entry.tunnels.clone(), gw.shutdown.clone()];
    let forwarded = reverse_proxy::forward(
        req,
        &Target::Tcp(target),
        gw.response_timeout,
        closes_on,
        |req| own_host = rewrite_request(req, target, peer),
    )
    .await;
    let mut resp = match forwarded {
        Ok(resp) => resp,
        Err(Upstream::Timeout) => {
            let text = format!("The dev server on 127.0.0.1:{target} sent no response in time.");
            return page(StatusCode::GATEWAY_TIMEOUT, "Preview timed out", &text);
        }
        Err(Upstream::Offline(error)) => {
            tracing::debug!(port = gw.port, target, %error, "preview target offline");
            return offline(target);
        }
    };
    rewrite_location(resp.headers_mut(), target, own_host.as_deref());
    resp
}

fn page(status: StatusCode, title: &str, text: &str) -> Response {
    let html =
        format!("<!doctype html><meta charset=\"utf-8\"><title>{title}</title><p>{text}</p>\n");
    (status, Html(html)).into_response()
}

fn offline(target: u16) -> Response {
    let html = format!(
        "<!doctype html><meta charset=\"utf-8\"><meta http-equiv=\"refresh\" content=\"3\">\
         <title>Preview offline</title>\
         <p>The dev server on 127.0.0.1:{target} is not responding. Retrying every 3 seconds.</p>\n"
    );
    (StatusCode::BAD_GATEWAY, Html(html)).into_response()
}

/// The preview's own rewrites, after the proxy's hop-by-hop strip and before its cookie fence.
/// Returns the browser-facing `Host`, which [`rewrite_location`] needs.
fn rewrite_request(req: &mut Request, target: u16, peer: SocketAddr) -> Option<String> {
    let headers = req.headers_mut();
    let target_origin = format!("http://127.0.0.1:{target}");
    let own_host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if let Some(host) = &own_host {
        // Only this preview's own origin is translated, so a dev server's same-origin check
        // (vite's proxy `changeOrigin`) still passes; any other origin reaches it unchanged.
        let own_origin = format!("http://{host}");
        let same = |v: &str| {
            v.get(..own_origin.len())
                .is_some_and(|p| p.eq_ignore_ascii_case(&own_origin))
                .then(|| v[own_origin.len()..].to_owned())
        };
        if let Some(rest) = headers
            .get(header::ORIGIN)
            .and_then(|v| v.to_str().ok())
            .and_then(same)
            && rest.is_empty()
            && let Ok(value) = HeaderValue::from_str(&target_origin)
        {
            headers.insert(header::ORIGIN, value);
        }
        if let Some(rest) = headers
            .get(header::REFERER)
            .and_then(|v| v.to_str().ok())
            .and_then(same)
            && (rest.is_empty() || rest.starts_with('/'))
            && let Ok(value) = HeaderValue::from_str(&format!("{target_origin}{rest}"))
        {
            headers.insert(header::REFERER, value);
        }
        if let Ok(value) = HeaderValue::from_str(host) {
            headers.insert("x-forwarded-host", value);
        }
    }
    headers.insert("x-forwarded-proto", HeaderValue::from_static("http"));
    if let Ok(value) = HeaderValue::from_str(&peer.ip().to_string()) {
        headers.insert("x-forwarded-for", value);
    }
    if let Ok(value) = HeaderValue::from_str(&format!("127.0.0.1:{target}")) {
        headers.insert(header::HOST, value);
    }
    own_host
}

fn rewrite_location(headers: &mut HeaderMap, target: u16, own_host: Option<&str>) {
    // Absolute on the preview origin, never relative: `//evil/x` as a path would otherwise
    // become a network-path reference (an open redirect).
    let Some(own_host) = own_host else { return };
    let absolute = headers
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|location| {
            // `localhost` covers a dev server that names itself so; both resolve to this target.
            ["127.0.0.1", "localhost"].iter().find_map(|host| {
                let rest = location.strip_prefix(&format!("http://{host}:{target}"))?;
                let path = match rest.chars().next() {
                    None => "/".to_owned(),
                    Some('/') => rest.to_owned(),
                    Some('?' | '#') => format!("/{rest}"),
                    Some(_) => return None,
                };
                Some(format!("http://{own_host}{path}"))
            })
        });
    if let Some(value) = absolute.and_then(|a| HeaderValue::from_str(&a).ok()) {
        headers.insert(header::LOCATION, value);
    }
}
