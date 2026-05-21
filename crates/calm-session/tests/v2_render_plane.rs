//! Acceptance tests for [`RenderPlane`] — the server-side bundle of
//! `TerminalModel` + transcript ByteRing introduced in PR-2.
//!
//! Verifies the divergence between `pty_seq` and `render_rev` and the
//! shape of broadcast effects.

use calm_session::terminal_model::ScrollbackLimit;
use calm_session::terminal_session::{Effect, RenderPlane};
use calm_session::{DaemonMsg, RenderEncoding};

#[test]
fn render_plane_pty_seq_and_rev_diverge_on_resize() {
    let mut rp = RenderPlane::new(80, 24, 4096, 100);
    assert_eq!(rp.pty_seq(), 0);
    assert_eq!(rp.render_rev(), 0);

    // Feed bytes: pty_seq++ AND render_rev++.
    let _ = rp.on_pty_chunk(b"a".to_vec());
    let seq_after_chunk = rp.pty_seq();
    let rev_after_chunk = rp.render_rev();
    assert_eq!(seq_after_chunk, 1);
    assert!(rev_after_chunk >= 1, "rev should bump on print");

    // Resize: render_rev++, pty_seq unchanged.
    let _ = rp.on_resize(40, 12);
    assert_eq!(
        rp.pty_seq(),
        seq_after_chunk,
        "resize must not bump pty_seq"
    );
    assert!(
        rp.render_rev() > rev_after_chunk,
        "resize must bump render_rev"
    );
}

#[test]
fn render_plane_on_pty_chunk_emits_broadcast_render_patch() {
    let mut rp = RenderPlane::new(80, 24, 4096, 100);
    let effects = rp.on_pty_chunk(b"hi".to_vec());
    assert!(
        effects.iter().any(|e| matches!(
            e,
            Effect::Broadcast(DaemonMsg::RenderPatch(p))
                if p.pty_seq == 1
                && p.prev_render_rev == 0
                && p.encoding == RenderEncoding::Vt
                && p.data == b"hi"
        )),
        "expected Broadcast(RenderPatch{{pty_seq=1,data=hi,Vt}}), got {effects:?}"
    );
}

#[test]
fn render_plane_build_snapshot_includes_visible_content() {
    let mut rp = RenderPlane::new(80, 24, 4096, 100);
    let _ = rp.on_pty_chunk(b"hello".to_vec());
    let snap = rp.build_snapshot(80, 24, ScrollbackLimit::None);
    let s = String::from_utf8_lossy(&snap.data);
    assert!(s.contains("hello"), "snapshot data missing 'hello': {s:?}");
    assert_eq!(snap.cols, 80);
    assert_eq!(snap.rows, 24);
    assert_eq!(snap.encoding, RenderEncoding::Vt);
    assert!(
        snap.scrollback.is_none(),
        "ScrollbackLimit::None must yield None scrollback"
    );
}

#[test]
fn render_plane_on_resize_emits_render_snapshot() {
    let mut rp = RenderPlane::new(80, 24, 4096, 100);
    let effects = rp.on_resize(40, 12);
    let snap = effects.iter().find_map(|e| match e {
        Effect::Broadcast(DaemonMsg::RenderSnapshot(s)) => Some(s),
        _ => None,
    });
    let snap = snap.expect("expected Broadcast(RenderSnapshot) from resize");
    assert_eq!(snap.cols, 40);
    assert_eq!(snap.rows, 12);
}

#[test]
fn render_plane_build_snapshot_with_scrollback_lines() {
    // Force scrollback to accumulate by feeding more lines than fit.
    let mut rp = RenderPlane::new(10, 2, 4096, 100);
    for i in 0..5 {
        rp.on_pty_chunk(format!("line{i}\n").into_bytes());
    }
    let snap = rp.build_snapshot(10, 2, ScrollbackLimit::Lines(3));
    assert!(
        snap.scrollback.is_some(),
        "Scrollback::Lines should populate scrollback field"
    );
}
