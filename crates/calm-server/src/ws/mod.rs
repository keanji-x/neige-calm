//! WebSocket route registry.
//!
//! `/api/events`        → ws::events    (track C)
//! `/api/terminals/:id` → ws::terminal  (track D)

use crate::state::AppState;
use axum::Router;

pub mod events;
pub mod terminal;

pub fn router() -> Router<AppState> {
    Router::new()
        .merge(events::router())
        .merge(terminal::router())
}
