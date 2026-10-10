//! The one reverse proxy from calm to a local upstream (a TCP port on loopback or a Unix socket):
//! request and response bodies stream, and an upgrade becomes a byte tunnel that closes when any of
//! its cancellation tokens fires. Every caller gets the same cookie fence: calm's session cookie
//! never leaves for the upstream, and an upstream can never set it.

use crate::auth::{SESSION_COOKIE, cookie_name, is_session_cookie};
use axum::body::Body;
use axum::extract::Request;
use axum::http::uri::PathAndQuery;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, Uri, Version, header};
use axum::response::Response;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
use hyper::upgrade::Upgraded;
use hyper_util::rt::TokioIo;
use std::path::PathBuf;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpStream, UnixStream};
use tokio_util::sync::CancellationToken;

/// An upstream that does not accept within this is offline.
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

/// Where a request is forwarded.
#[derive(Debug, Clone)]
pub enum Target {
    /// `127.0.0.1:<port>`.
    Tcp(u16),
    Unix(PathBuf),
}

/// Why no upstream response came back.
#[derive(Debug)]
pub enum Upstream {
    /// Not accepting, or the connection or exchange failed.
    Offline(anyhow::Error),
    /// Connected, but no response headers within the response timeout.
    Timeout,
}

/// Forwards `req` to `target` as origin-form HTTP/1.1 and returns the upstream's response.
///
/// Before sending, the request loses its hop-by-hop headers (an upgrade keeps its pair), then
/// `rewrite` applies the caller's own header changes, then calm's session cookie is removed, so no
/// rewrite can put it back. The response loses its hop-by-hop headers and every `Set-Cookie` for
/// calm's session. A `101` to an upgrade request becomes a byte tunnel that closes when any token in
/// `closes_on` is cancelled (at once, if there are none).
pub async fn forward(
    mut req: Request,
    target: &Target,
    response_timeout: Duration,
    closes_on: Vec<CancellationToken>,
    rewrite: impl FnOnce(&mut Request),
) -> Result<Response, Upstream> {
    let upgrade = is_upgrade(req.headers());
    let client_upgrade = upgrade.then(|| hyper::upgrade::on(&mut req));
    prepare_request(&mut req);
    rewrite(&mut req);
    strip_session_cookie(req.headers_mut());
    let mut resp = send(req, target, response_timeout).await?;
    let switching = resp.status() == StatusCode::SWITCHING_PROTOCOLS;
    if switching && let Some(client) = client_upgrade {
        let upstream = hyper::upgrade::on(&mut resp);
        tokio::spawn(tunnel(client, upstream, closes_on));
    }
    rewrite_response(resp.headers_mut(), switching);
    Ok(resp.map(Body::new))
}

/// Joins both upgraded halves into a byte tunnel that lives until either side closes or a token
/// in `closes_on` is cancelled.
async fn tunnel(
    client: impl Future<Output = hyper::Result<Upgraded>>,
    upstream: impl Future<Output = hyper::Result<Upgraded>>,
    closes_on: Vec<CancellationToken>,
) {
    let mut closed: FuturesUnordered<_> =
        closes_on.iter().map(CancellationToken::cancelled).collect();
    // Raced against the tokens too: a half that never completes its upgrade must not hold the
    // other one open past cancellation.
    let joined = tokio::select! {
        _ = closed.next() => return,
        joined = async { tokio::try_join!(client, upstream) } => joined,
    };
    let Ok((client, upstream)) = joined else {
        return;
    };
    let (mut client, mut upstream) = (TokioIo::new(client), TokioIo::new(upstream));
    tokio::select! {
        _ = closed.next() => {}
        _ = tokio::io::copy_bidirectional(&mut client, &mut upstream) => {}
    }
}

async fn send(
    req: Request,
    target: &Target,
    response_timeout: Duration,
) -> Result<axum::http::Response<hyper::body::Incoming>, Upstream> {
    let offline = |e: anyhow::Error| Upstream::Offline(e);
    match target {
        Target::Tcp(port) => {
            let stream = connect(TcpStream::connect(("127.0.0.1", *port))).await?;
            exchange(stream, req, target, response_timeout).await
        }
        Target::Unix(path) => {
            let stream = connect(UnixStream::connect(path))
                .await
                .map_err(|e| match e {
                    Upstream::Offline(e) => offline(e.context(path.display().to_string())),
                    other => other,
                })?;
            exchange(stream, req, target, response_timeout).await
        }
    }
}

async fn connect<S>(connecting: impl Future<Output = std::io::Result<S>>) -> Result<S, Upstream> {
    tokio::time::timeout(CONNECT_TIMEOUT, connecting)
        .await
        .map_err(|e| Upstream::Offline(e.into()))?
        .map_err(|e| Upstream::Offline(e.into()))
}

async fn exchange<S>(
    stream: S,
    req: Request,
    target: &Target,
    response_timeout: Duration,
) -> Result<axum::http::Response<hyper::body::Incoming>, Upstream>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let offline = |e: anyhow::Error| Upstream::Offline(e);
    let (mut sender, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| offline(e.into()))?;
    let target = target.clone();
    tokio::spawn(async move {
        if let Err(error) = conn.with_upgrades().await {
            tracing::debug!(?target, %error, "proxied upstream connection ended");
        }
    });
    tokio::time::timeout(response_timeout, sender.send_request(req))
        .await
        .map_err(|_| Upstream::Timeout)?
        .map_err(|e| offline(e.into()))
}

/// `Connection: upgrade` plus an `Upgrade` header.
pub fn is_upgrade(headers: &HeaderMap) -> bool {
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

fn prepare_request(req: &mut Request) {
    let path = req
        .uri()
        .path_and_query()
        .cloned()
        .unwrap_or_else(|| PathAndQuery::from_static("/"));
    *req.uri_mut() = Uri::from(path);
    *req.version_mut() = Version::HTTP_11;
    let upgrade = is_upgrade(req.headers());
    strip_hop_by_hop(req.headers_mut(), upgrade);
}

/// Browser cookies ignore ports and paths here: calm's session is in the same jar as everything
/// an upstream on this host receives, and must never leave. Every other cookie passes.
fn strip_session_cookie(headers: &mut HeaderMap) {
    let kept = header_tokens(headers, header::COOKIE, ';')
        .filter(|c| !c.is_empty() && !is_session_cookie(c))
        .collect::<Vec<_>>()
        .join("; ");
    headers.remove(header::COOKIE);
    if !kept.is_empty()
        && let Ok(value) = HeaderValue::from_str(&kept)
    {
        headers.insert(header::COOKIE, value);
    }
}

/// An upstream must not set calm's session, raw or percent-encoded (`calm%2Dsession`), in any of
/// the cookie-prefix spellings. A nameless cookie (`=calm-session=X`) counts too: a browser stores
/// it by its value and sends that back as `calm-session=X`.
fn sets_calm_session(set_cookie: &str) -> bool {
    let pair = set_cookie.split(';').next().unwrap_or_default();
    let mut names = vec![cookie_name(pair)];
    if names[0].is_empty()
        && let Some((_, value)) = pair.split_once('=')
    {
        names.push(cookie_name(value));
    }
    names
        .iter()
        .flat_map(|raw| [raw.to_string(), percent_decode(raw)])
        .any(|name| {
            let name = name
                .strip_prefix("__Host-")
                .or_else(|| name.strip_prefix("__Secure-"))
                .unwrap_or(&name);
            name == SESSION_COOKIE
        })
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .filter(|h| h.iter().all(u8::is_ascii_hexdigit))
            .and_then(|h| std::str::from_utf8(h).ok());
        match (bytes[i], hex.and_then(|h| u8::from_str_radix(h, 16).ok())) {
            (b'%', Some(byte)) => {
                out.push(byte);
                i += 3;
            }
            (byte, _) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn rewrite_response(headers: &mut HeaderMap, switching: bool) {
    strip_hop_by_hop(headers, switching);
    let cookies: Vec<HeaderValue> = headers
        .get_all(header::SET_COOKIE)
        .into_iter()
        // Fail closed: a Set-Cookie that is not visible ASCII cannot be checked, so it drops.
        .filter(|v| v.to_str().is_ok_and(|s| !sets_calm_session(s)))
        .cloned()
        .collect();
    headers.remove(header::SET_COOKIE);
    for cookie in cookies {
        headers.append(header::SET_COOKIE, cookie);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An upgrade half that never completes (a client that never finishes its upgrade) must not
    /// keep the other half alive past cancellation.
    #[tokio::test]
    async fn cancellation_releases_a_tunnel_whose_upgrade_never_completes() {
        let (held, released) = tokio::sync::oneshot::channel::<()>();
        let upstream = async move {
            let _held = held;
            std::future::pending::<hyper::Result<Upgraded>>().await
        };
        let stop = CancellationToken::new();
        let task = tokio::spawn(tunnel(std::future::pending(), upstream, vec![stop.clone()]));
        stop.cancel();
        tokio::time::timeout(Duration::from_secs(5), released)
            .await
            .expect("cancellation must drop the pending upstream half")
            .unwrap_err();
        task.await.unwrap();
    }
}
