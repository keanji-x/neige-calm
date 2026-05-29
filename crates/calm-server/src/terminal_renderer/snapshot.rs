use calm_session::terminal_model::ScrollbackLimit;
use calm_session::{DaemonMsg, PtySize};

use super::SharedRenderPlane;

pub fn scrollback_request(req: calm_session::InitialScrollback) -> ScrollbackLimit {
    match req {
        calm_session::InitialScrollback::None => ScrollbackLimit::None,
        calm_session::InitialScrollback::All => ScrollbackLimit::All,
        calm_session::InitialScrollback::Lines(n) => ScrollbackLimit::Lines(n),
    }
}

// Copied from crates/calm-session/src/bin/daemon.rs::rebuild_server_hello_snapshot as part of #388 Phase 3a lift. Daemon binary retires in 3c; until then we live with duplication.
pub fn rebuild_server_hello_snapshot(
    msg: DaemonMsg,
    render_plane: &SharedRenderPlane,
    desired_size: Option<PtySize>,
    scrollback: Option<ScrollbackLimit>,
) -> DaemonMsg {
    match msg {
        DaemonMsg::ServerHello {
            protocol_version,
            terminal_id,
            session_id,
            client_role,
            owner_client_id,
            pty_size,
            pty_seq_head,
            pty_seq_tail,
            render_rev,
            snapshot: _,
            history_gap,
            is_child_ready,
        } => {
            let (cols, rows) = if client_role == calm_session::Role::Owner {
                desired_size
                    .map(|s| (s.cols, s.rows))
                    .unwrap_or((pty_size.cols, pty_size.rows))
            } else {
                (pty_size.cols, pty_size.rows)
            };
            let limit = scrollback.unwrap_or(ScrollbackLimit::None);
            let snapshot = match render_plane.lock() {
                Ok(rp) => rp.build_snapshot(cols, rows, limit),
                Err(_) => {
                    tracing::warn!("render_plane lock poisoned; sending empty snapshot");
                    calm_session::RenderSnapshot {
                        render_rev,
                        pty_seq: pty_seq_tail,
                        cols,
                        rows,
                        encoding: calm_session::RenderEncoding::Vt,
                        data: Vec::new(),
                        scrollback: None,
                    }
                }
            };
            DaemonMsg::ServerHello {
                protocol_version,
                terminal_id,
                session_id,
                client_role,
                owner_client_id,
                pty_size,
                pty_seq_head,
                pty_seq_tail,
                render_rev,
                snapshot,
                history_gap,
                is_child_ready,
            }
        }
        other => other,
    }
}
