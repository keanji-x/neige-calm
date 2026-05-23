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
fn render_plane_osc_11_query_emits_write_to_pty_with_reply() {
    // #177: when the model has a default_bg configured and the child
    // probes via OSC 11, `on_pty_chunk` must surface a `WriteToPty`
    // effect carrying the reply alongside the usual `RenderPatch`
    // broadcast.
    let mut rp = RenderPlane::with_colors(80, 24, 4096, 100, None, Some((17, 20, 24)));
    let effects = rp.on_pty_chunk(b"\x1b]11;?\x1b\\".to_vec());

    let has_patch = effects.iter().any(|e| {
        matches!(
            e,
            Effect::Broadcast(DaemonMsg::RenderPatch(p)) if p.encoding == RenderEncoding::Vt,
        )
    });
    assert!(has_patch, "expected RenderPatch broadcast, got {effects:?}");

    let write_data = effects
        .iter()
        .find_map(|e| match e {
            Effect::WriteToPty { data, .. } => Some(data.clone()),
            _ => None,
        })
        .expect("expected WriteToPty with OSC 11 reply");
    assert!(
        write_data.starts_with(b"\x1b]11;rgb:"),
        "expected OSC 11 reply prefix, got {write_data:?}",
    );
}

#[test]
fn render_plane_without_default_colors_stays_silent_on_osc_query() {
    // No `with_colors` → no reply, just the usual RenderPatch broadcast.
    // Locks the back-compat path: a daemon spawned without
    // `--terminal-fg`/`--terminal-bg` continues to behave like pre-#177.
    let mut rp = RenderPlane::new(80, 24, 4096, 100);
    let effects = rp.on_pty_chunk(b"\x1b]11;?\x1b\\".to_vec());
    assert!(
        !effects
            .iter()
            .any(|e| matches!(e, Effect::WriteToPty { .. })),
        "without default colors there must be no WriteToPty, got {effects:?}"
    );
}

#[test]
fn render_plane_set_default_colors_takes_effect_for_next_query() {
    // Mid-session theme toggle path (#177): the kernel-input-authorized
    // ClientMsg::TerminalThemeUpdate eventually calls into
    // `set_default_colors`. Verify a subsequent OSC query reflects the
    // new value.
    let mut rp = RenderPlane::new(80, 24, 4096, 100);
    rp.set_default_colors(Some((216, 219, 226)), Some((15, 20, 24)));
    let effects = rp.on_pty_chunk(b"\x1b]10;?\x1b\\".to_vec());
    let write = effects
        .iter()
        .find_map(|e| match e {
            Effect::WriteToPty { data, .. } => Some(data.clone()),
            _ => None,
        })
        .expect("expected WriteToPty");
    assert!(write.starts_with(b"\x1b]10;rgb:"));
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
