//! `GET /api/plugins/{id}/ws/{*path}` (WebSocket upgrade only): tunnels the upgrade to the Unix
//! socket the plugin declares in its manifest (`http_socket`, manifest version 6), with the rest
//! of the path and the query, through [`crate::reverse_proxy`]. The WS router's
//! `require_session_ws` has already checked the session and the upgrade's `Origin`; the proxy
//! strips calm's session cookie and drops any `Set-Cookie` for it.
//!
//! Plain HTTP is never proxied: pages a plugin generates would run on calm's own origin.
//!
//! - 400 `bad_request`: not a WebSocket upgrade.
//! - 404 `not_found`: no installed plugin with this id declares an `http_socket`.
//! - 503 `service_unavailable`: the plugin is not serving (not enabled, starting, stopping or
//!   crashed), or its socket does not answer the upgrade.
//! - 502, with no headers or body of the plugin's: the plugin answered with anything but `101`.
//!
//! The plugin receives no `Forwarded` or `X-Forwarded-*` header; `Host` and `Origin` are the
//! client's and must not be trusted.
//!
//! Every tunnel closes when the plugin's run stops serving: stop, disable, reload, restart, crash.

use crate::error::{CalmError, Result};
use crate::extract::Path;
use crate::plugin_host::SocketUnavailable;
use crate::reverse_proxy::{self, Target};
use crate::state::AppState;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response};
use axum::{Router, routing::get};
use std::time::Duration;

/// A plugin answers the upgrade on its own socket; anything slower is treated as not serving.
const UPGRADE_TIMEOUT: Duration = Duration::from_secs(10);

pub fn router() -> Router<AppState> {
    Router::new().route("/api/plugins/{id}/ws/{*path}", get(upgrade))
}

async fn upgrade(
    State(s): State<AppState>,
    Path((id, _path)): Path<(String, String)>,
    mut req: Request,
) -> Result<Response> {
    if !is_websocket_upgrade(req.headers()) {
        return Err(CalmError::BadRequest(
            "this route accepts WebSocket upgrades only".into(),
        ));
    }
    let socket = s.plugin.http_socket(&id).map_err(|why| match why {
        SocketUnavailable::NotDeclared => {
            CalmError::NotFound(format!("plugin `{id}` declares no http_socket"))
        }
        SocketUnavailable::NotRunning => {
            CalmError::ServiceUnavailable(format!("plugin `{id}` is not running"))
        }
    })?;
    *req.uri_mut() = upstream_uri(req.uri())
        .ok_or_else(|| CalmError::BadRequest("malformed plugin socket path".into()))?;
    let target = Target::Unix(socket.path);
    let resp = reverse_proxy::forward(
        req,
        &target,
        UPGRADE_TIMEOUT,
        vec![socket.serving],
        strip_client_forwarding,
    )
    .await
    .map_err(|error| {
        tracing::debug!(plugin_id = %id, ?error, "plugin socket did not answer");
        CalmError::ServiceUnavailable(format!("plugin `{id}` did not answer on its socket"))
    })?;
    if resp.status() != StatusCode::SWITCHING_PROTOCOLS {
        // Anything but a switch would be a plugin page on calm's origin. Dropping `resp` here
        // drops its body and, with it, the plugin connection; nothing of it reaches the client.
        tracing::debug!(plugin_id = %id, status = %resp.status(), "plugin refused the upgrade");
        return Ok(StatusCode::BAD_GATEWAY.into_response());
    }
    Ok(resp)
}

/// A client may send any `Forwarded` or `X-Forwarded-*` value; none of them reaches a plugin.
fn strip_client_forwarding(req: &mut Request) {
    let headers = req.headers_mut();
    let forged: Vec<HeaderName> = headers
        .keys()
        .filter(|name| *name == header::FORWARDED || name.as_str().starts_with("x-forwarded-"))
        .cloned()
        .collect();
    for name in forged {
        headers.remove(name);
    }
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    reverse_proxy::is_upgrade(headers)
        && headers
            .get(header::UPGRADE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.trim().eq_ignore_ascii_case("websocket"))
}

/// `/api/plugins/<id>/ws/<rest>?<query>` → `/<rest>?<query>`, taken from the raw request so the
/// rest keeps its percent-encoding.
fn upstream_uri(uri: &Uri) -> Option<Uri> {
    // `nth(5)` is tied to the route shape in `router`: "", "api", "plugins", "{id}", "ws", rest.
    // The router merges (never nests) this route, so `uri` is the full request path.
    let rest = uri.path().splitn(6, '/').nth(5)?;
    let path_and_query = match uri.query() {
        Some(query) => format!("/{rest}?{query}"),
        None => format!("/{rest}"),
    };
    Uri::try_from(path_and_query).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rest_of_the_path_and_the_query_reach_the_plugin_still_encoded() {
        for (from, to) in [
            ("/api/plugins/p/ws/apps/one/stream", "/apps/one/stream"),
            ("/api/plugins/p/ws/a%2Fb?x=1&y=%20", "/a%2Fb?x=1&y=%20"),
            ("/api/plugins/p/ws/s", "/s"),
        ] {
            let uri: Uri = from.parse().unwrap();
            assert_eq!(upstream_uri(&uri).unwrap(), to, "{from}");
        }
    }
}
