//! One listener per pool port; every request is reverse-proxied to that port's registered
//! `127.0.0.1` target. Bodies stream both ways; an upgrade (vite HMR) becomes a byte tunnel.
//!
//! The pool is reachable from the LAN and `Host` is rewritten to loopback (which defeats a dev
//! server's own DNS-rebinding checks), so every request must carry calm's session first; an
//! unauthenticated request never reaches the target.

use super::PreviewRegistry;
use crate::auth::{AuthState, SESSION_COOKIE, resolve_principal};
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::uri::PathAndQuery;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, Version, header};
use axum::response::{Html, IntoResponse, Response};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// A dev server that does not accept within this is reported offline.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
/// Wait for response headers; generous because vite's first on-demand compile can be slow.
pub const RESPONSE_TIMEOUT: Duration = Duration::from_secs(60);

const HOP_BY_HOP: [HeaderName; 8] = [
    header::CONNECTION,
    HeaderName::from_static("keep-alive"),
    header::PROXY_AUTHENTICATE,
    header::PROXY_AUTHORIZATION,
    header::TE,
    header::TRAILER,
    header::TRANSFER_ENCODING,
    header::UPGRADE,
];

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
    mut req: Request,
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
    let upgrade = is_upgrade(req.headers());
    let client_upgrade = upgrade.then(|| hyper::upgrade::on(&mut req));
    let own_host = rewrite_request(&mut req, target, peer);
    let mut resp = match forward(req, target, gw.response_timeout).await {
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
    let switching = resp.status() == StatusCode::SWITCHING_PROTOCOLS;
    if switching && let Some(client) = client_upgrade {
        let upstream = hyper::upgrade::on(&mut resp);
        // Tied to the registration and the gateway: unregister, re-target or shutdown closes it.
        let (registration, shutdown) = (entry.tunnels.clone(), gw.shutdown.clone());
        tokio::spawn(async move {
            let Ok((client, upstream)) = tokio::try_join!(client, upstream) else {
                return;
            };
            let (mut client, mut upstream) = (TokioIo::new(client), TokioIo::new(upstream));
            tokio::select! {
                _ = registration.cancelled() => {}
                _ = shutdown.cancelled() => {}
                _ = tokio::io::copy_bidirectional(&mut client, &mut upstream) => {}
            }
        });
    }
    rewrite_response(resp.headers_mut(), target, own_host.as_deref(), switching);
    resp.map(Body::new)
}

enum Upstream {
    Offline(anyhow::Error),
    Timeout,
}

async fn forward(
    req: Request,
    target: u16,
    response_timeout: Duration,
) -> Result<axum::http::Response<hyper::body::Incoming>, Upstream> {
    let offline = |e: anyhow::Error| Upstream::Offline(e);
    let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(("127.0.0.1", target)))
        .await
        .map_err(|e| offline(e.into()))?
        .map_err(|e| offline(e.into()))?;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| offline(e.into()))?;
    tokio::spawn(async move {
        if let Err(error) = conn.with_upgrades().await {
            tracing::debug!(target, %error, "preview upstream connection ended");
        }
    });
    tokio::time::timeout(response_timeout, sender.send_request(req))
        .await
        .map_err(|_| Upstream::Timeout)?
        .map_err(|e| offline(e.into()))
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

fn is_upgrade(headers: &HeaderMap) -> bool {
    headers.contains_key(header::UPGRADE)
        && header_tokens(headers, header::CONNECTION, ',')
            .any(|t| t.eq_ignore_ascii_case("upgrade"))
}

fn header_tokens(headers: &HeaderMap, name: HeaderName, sep: char) -> impl Iterator<Item = &str> {
    headers
        .get_all(name)
        .into_iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(move |v| v.split(sep))
        .map(str::trim)
}

/// Removes hop-by-hop headers, including those `Connection` names; an upgrade keeps its pair.
fn strip_hop_by_hop(headers: &mut HeaderMap, keep_upgrade: bool) {
    let named: Vec<HeaderName> = header_tokens(headers, header::CONNECTION, ',')
        .filter_map(|t| HeaderName::from_bytes(t.as_bytes()).ok())
        .collect();
    for name in named.iter().chain(HOP_BY_HOP.iter()) {
        if !(keep_upgrade && name == header::UPGRADE) {
            headers.remove(name);
        }
    }
    if keep_upgrade {
        headers.insert(header::CONNECTION, HeaderValue::from_static("upgrade"));
    }
}

/// Returns the browser-facing `Host`, which [`rewrite_response`] needs for `Location`.
fn rewrite_request(req: &mut Request, target: u16, peer: SocketAddr) -> Option<String> {
    let path = req
        .uri()
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| PathAndQuery::from_static("/"));
    *req.uri_mut() = Uri::from(path);
    *req.version_mut() = Version::HTTP_11;
    let upgrade = is_upgrade(req.headers());
    let headers = req.headers_mut();
    strip_hop_by_hop(headers, upgrade);
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
    // Browser cookies ignore ports: calm's session is in this jar and must never leave. Every
    // other cookie is shared by all previews, which are the owner's own dev servers.
    let kept = header_tokens(headers, header::COOKIE, ';')
        .filter(|c| !c.is_empty() && cookie_name(c) != SESSION_COOKIE)
        .collect::<Vec<_>>()
        .join("; ");
    headers.remove(header::COOKIE);
    if !kept.is_empty()
        && let Ok(value) = HeaderValue::from_str(&kept)
    {
        headers.insert(header::COOKIE, value);
    }
    own_host
}

fn cookie_name(pair: &str) -> &str {
    pair.split_once('=').map_or(pair, |(name, _)| name).trim()
}

/// A dev server must not overwrite calm's session, in any of the cookie-prefix spellings.
fn sets_calm_session(set_cookie: &str) -> bool {
    let name = cookie_name(set_cookie);
    let name = name
        .strip_prefix("__Host-")
        .or_else(|| name.strip_prefix("__Secure-"))
        .unwrap_or(name);
    name == SESSION_COOKIE
}

fn rewrite_response(headers: &mut HeaderMap, target: u16, own_host: Option<&str>, switching: bool) {
    strip_hop_by_hop(headers, switching);
    let cookies: Vec<HeaderValue> = headers
        .get_all(header::SET_COOKIE)
        .into_iter()
        .filter(|v| !v.to_str().is_ok_and(sets_calm_session))
        .cloned()
        .collect();
    headers.remove(header::SET_COOKIE);
    for cookie in cookies {
        headers.append(header::SET_COOKIE, cookie);
    }
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
