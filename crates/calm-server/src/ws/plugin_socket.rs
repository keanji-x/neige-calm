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
//!
//! Every tunnel closes when the plugin's run stops serving: stop, disable, reload, restart, crash.

use crate::error::{CalmError, Result};
use crate::extract::Path;
use crate::plugin_host::SocketUnavailable;
use crate::reverse_proxy::{self, Target};
use crate::state::AppState;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, Uri, header};
use axum::response::Response;
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
    reverse_proxy::forward(req, &target, UPGRADE_TIMEOUT, vec![socket.serving], |_| {})
        .await
        .map_err(|error| {
            tracing::debug!(plugin_id = %id, ?error, "plugin socket did not answer");
            CalmError::ServiceUnavailable(format!("plugin `{id}` did not answer on its socket"))
        })
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
