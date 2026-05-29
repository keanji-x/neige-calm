use std::time::Duration;

use calm_session::DaemonMsg;
use calm_session::terminal_session::Effect;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use super::SharedRenderPlane;

// Copied from crates/calm-session/src/bin/daemon.rs::run_terminal child-ready poller as part of #388 Phase 3a lift. Daemon binary retires in 3c; until then we live with duplication.
pub fn spawn_child_ready_poller(
    render_plane: SharedRenderPlane,
    event_tx: broadcast::Sender<DaemonMsg>,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        tick.tick().await;
        loop {
            tick.tick().await;
            let effect = match render_plane.lock() {
                Ok(mut rp) => rp.detect_ready(),
                Err(_) => None,
            };
            if let Some(Effect::Broadcast(msg)) = effect {
                let _ = event_tx.send(msg);
                break;
            }
        }
    })
}
