//! WebSocket route registry.
//!
//! `/api/events`        → ws::events    (track C)
//! `/api/terminals/:id` → ws::terminal  (track D)
//! `/api/plugins/{id}/ws/{*path}` → ws::plugin_socket (#2530)

use crate::state::AppState;
use axum::Router;

pub mod events;
pub mod plugin_socket;
pub mod terminal;

pub fn router() -> Router<AppState> {
    Router::new()
        .merge(events::router())
        .merge(terminal::router())
        .merge(plugin_socket::router())
}
