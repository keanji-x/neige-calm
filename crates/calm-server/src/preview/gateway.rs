//! One listener per pool port; every request is reverse-proxied to that port's registered
//! `127.0.0.1` target. Bodies stream both ways; an upgrade (vite HMR) becomes a byte tunnel.

use super::PreviewRegistry;
use crate::auth::SESSION_COOKIE;
use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::uri::PathAndQuery;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, header};
use axum::response::{Html, IntoResponse, Response};
use hyper_util::rt::TokioIo;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;

/// A dev server that does not accept within this is reported offline.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

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
    port: u16,
}

/// Binds every pool port on `host` (a bind failure is a boot error) and serves until `shutdown`.
pub async fn spawn(
    registry: Arc<PreviewRegistry>,
    host: &str,
    shutdown: CancellationToken,
) -> anyhow::Result<()> {
    for &port in registry.pool() {
        let listener = TcpListener::bind((host, port))
            .await
            .map_err(|e| anyhow::anyhow!("preview gateway bind {host}:{port}: {e}"))?;
        serve(listener, registry.clone(), shutdown.clone())?;
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
    shutdown: CancellationToken,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    let port = listener.local_addr()?.port();
    let app = axum::Router::new()
        .fallback(proxy)
        .with_state(Gateway { registry, port });
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
    let Some(entry) = gw.registry.lookup(gw.port) else {
        let body = format!("no preview is registered on port {}\n", gw.port);
        return (StatusCode::NOT_FOUND, body).into_response();
    };
    let target = entry.target_port;
    let upgrade = is_upgrade(req.headers());
    let client_upgrade = upgrade.then(|| hyper::upgrade::on(&mut req));
    rewrite_request(&mut req, gw.port, target, peer);
    let mut resp = match forward(req, target).await {
        Ok(resp) => resp,
        Err(error) => {
            tracing::debug!(port = gw.port, target, %error, "preview target offline");
            return offline(target);
        }
    };
    let switching = resp.status() == StatusCode::SWITCHING_PROTOCOLS;
    if switching && let Some(client) = client_upgrade {
        let upstream = hyper::upgrade::on(&mut resp);
        tokio::spawn(async move {
            if let (Ok(client), Ok(upstream)) = tokio::join!(client, upstream) {
                let (mut client, mut upstream) = (TokioIo::new(client), TokioIo::new(upstream));
                let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
            }
        });
    }
    rewrite_response(resp.headers_mut(), gw.port, target, switching);
    resp.map(Body::new)
}

async fn forward(
    req: Request,
    target: u16,
) -> anyhow::Result<axum::http::Response<hyper::body::Incoming>> {
    let stream =
        tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(("127.0.0.1", target))).await??;
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    tokio::spawn(async move {
        if let Err(error) = conn.with_upgrades().await {
            tracing::debug!(target, %error, "preview upstream connection ended");
        }
    });
    Ok(sender.send_request(req).await?)
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
        && header_tokens(headers, header::CONNECTION).any(|t| t.eq_ignore_ascii_case("upgrade"))
}

fn header_tokens(headers: &HeaderMap, name: HeaderName) -> impl Iterator<Item = &str> {
    headers
        .get_all(name)
        .into_iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(str::trim)
}

/// Removes hop-by-hop headers, including those `Connection` names; an upgrade keeps its pair.
fn strip_hop_by_hop(headers: &mut HeaderMap, keep_upgrade: bool) {
    let named: Vec<HeaderName> = header_tokens(headers, header::CONNECTION)
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

fn rewrite_request(req: &mut Request, pool_port: u16, target: u16, peer: SocketAddr) {
    let path = req
        .uri()
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| PathAndQuery::from_static("/"));
    *req.uri_mut() = Uri::from(path);
    let upgrade = is_upgrade(req.headers());
    let headers = req.headers_mut();
    strip_hop_by_hop(headers, upgrade);
    let target_origin = format!("http://127.0.0.1:{target}");
    let own_host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    if let Some(host) = own_host {
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
        if let Ok(value) = HeaderValue::from_str(&host) {
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
    rewrite_request_cookies(headers, pool_port);
}

/// Browser cookies ignore ports, so every preview and calm share one jar: calm's session never
/// leaves, this port's `pv<port>_` cookies lose the prefix, other ports' prefixed cookies drop.
fn rewrite_request_cookies(headers: &mut HeaderMap, pool_port: u16) {
    let own = format!("pv{pool_port}_");
    let kept: Vec<String> = header_tokens_by(headers, header::COOKIE, ';')
        .filter(|c| !c.is_empty())
        .filter_map(|c| {
            let name = c.split_once('=').map_or(c, |(name, _)| name);
            if name == SESSION_COOKIE {
                None
            } else if let Some(unprefixed) = c.strip_prefix(&own) {
                Some(unprefixed.to_owned())
            } else if is_preview_prefixed(name) {
                None
            } else {
                Some(c.to_owned())
            }
        })
        .collect();
    headers.remove(header::COOKIE);
    if !kept.is_empty()
        && let Ok(value) = HeaderValue::from_str(&kept.join("; "))
    {
        headers.insert(header::COOKIE, value);
    }
}

fn header_tokens_by(
    headers: &HeaderMap,
    name: HeaderName,
    sep: char,
) -> impl Iterator<Item = &str> {
    headers
        .get_all(name)
        .into_iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(move |v| v.split(sep))
        .map(str::trim)
}

fn is_preview_prefixed(name: &str) -> bool {
    name.strip_prefix("pv")
        .and_then(|rest| rest.split_once('_'))
        .is_some_and(|(digits, _)| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

fn rewrite_response(headers: &mut HeaderMap, pool_port: u16, target: u16, switching: bool) {
    strip_hop_by_hop(headers, switching);
    let cookies: Vec<HeaderValue> = headers
        .get_all(header::SET_COOKIE)
        .into_iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| HeaderValue::from_str(&format!("pv{pool_port}_{}", v.trim_start())).ok())
        .collect();
    headers.remove(header::SET_COOKIE);
    for cookie in cookies {
        headers.append(header::SET_COOKIE, cookie);
    }
    let relative = headers
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|location| {
            ["127.0.0.1", "localhost"].iter().find_map(|host| {
                let rest = location.strip_prefix(&format!("http://{host}:{target}"))?;
                match rest.chars().next() {
                    None => Some("/".to_owned()),
                    Some('/') => Some(rest.to_owned()),
                    Some('?' | '#') => Some(format!("/{rest}")),
                    Some(_) => None,
                }
            })
        });
    if let Some(value) = relative.and_then(|r| HeaderValue::from_str(&r).ok()) {
        headers.insert(header::LOCATION, value);
    }
}
