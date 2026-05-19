//! HTTP route registry. Each sub-module (`coves`, `waves`, ...) returns its
//! own `Router<AppState>`; this file merges them.

use crate::state::AppState;
use axum::Router;

pub mod cards;
pub mod coves;
pub mod overlays;
pub mod plugins;
pub mod terminal;
pub mod waves;

pub fn router() -> Router<AppState> {
    Router::new()
        .merge(coves::router())
        .merge(waves::router())
        .merge(cards::router())
        .merge(overlays::router())
        .merge(plugins::router())
        .merge(terminal::router())
}
