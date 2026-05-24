//! Acceptance tests for the server-side [`TerminalModel`] (PR-2).
//!
//! These exercise the VTE-driven grid + scrollback + snapshot serializer
//! without any IO. All cases are deterministic and complete in
//! microseconds.

use calm_session::terminal_model::{Cursor, ScrollbackLimit, TerminalModel};

#[test]
fn model_feeds_basic_ansi() {
    // Feed "hello\nworld" with explicit cursor moves: should put "hello"
    // at row 0 starting col 0, "world" at row 1 starting col 0. The
    // snapshot must contain both substrings.
    let mut m = TerminalModel::new(20, 5, 100);
    m.feed(b"hello\x1b[2;1Hworld");
    let snap = m.snapshot_vt(20, 5);
    let s = String::from_utf8_lossy(&snap);
    assert!(s.contains("hello"), "snapshot missing 'hello': {s:?}");
    assert!(s.contains("world"), "snapshot missing 'world': {s:?}");
}

#[test]
fn model_resize_preserves_grid_within_bounds() {
    // Write "hello" at top-left, resize from 80x24 to 40x12; cursor must
    // still be in-bounds and the text still appear in the snapshot.
    let mut m = TerminalModel::new(80, 24, 100);
    m.feed(b"hello");
    m.resize(40, 12);
    let (cols, rows) = m.size();
    assert_eq!((cols, rows), (40, 12));
    let cur = m.cursor();
    assert!(cur.row < rows && cur.col < cols, "cursor OOB: {cur:?}");
    let snap = m.snapshot_vt(40, 12);
    assert!(
        String::from_utf8_lossy(&snap).contains("hello"),
        "post-resize snapshot missing 'hello'"
    );
}

#[test]
fn render_rev_monotonic_only_on_state_change() {
    let mut m = TerminalModel::new(80, 24, 100);
    let r0 = m.rev();
    m.feed(b"");
    assert_eq!(m.rev(), r0, "empty feed must not bump rev");

    m.feed(b"a");
    assert!(m.rev() > r0, "printing must bump rev");

    let r1 = m.rev();
    // A NUL byte is a noop in `execute` — must not bump.
    m.feed(b"\0");
    assert_eq!(m.rev(), r1, "noop byte (NUL) must not bump rev");
}

#[test]
fn sgr_state_tracks_csi() {
    // Feed "\x1b[31mred\x1b[0m" — print red text then reset. After the
    // reset, the next char should have default SGR.
    let mut m = TerminalModel::new(10, 1, 100);
    m.feed(b"\x1b[31mred\x1b[0mx");
    // Last printed char 'x' should be at col 3 with default SGR.
    let snap = m.snapshot_vt(10, 1);
    let s = String::from_utf8_lossy(&snap);
    // The snapshot must contain the SGR sequence for red (param 31).
    assert!(s.contains("31m"), "snapshot missing 'red' SGR: {s:?}");
    assert!(s.contains("red"), "snapshot missing 'red' text: {s:?}");
    assert!(s.contains('x'), "snapshot missing trailing 'x'");
}

#[test]
fn scrollback_grows_on_lf_overflow() {
    // cols=10, rows=2: write 5 lines of "abc\n". After the first 2,
    // each LF scrolls one line into scrollback. So we expect 3 lines
    // in scrollback.
    let mut m = TerminalModel::new(10, 2, 100);
    m.feed(b"a\nb\nc\nd\ne\n");
    let sb = m.scrollback_vt(ScrollbackLimit::All);
    // We can't easily assert the line count from the byte stream, but
    // we can confirm "a" (the earliest line) appears in the scrollback
    // stream (must have been scrolled out).
    let s = String::from_utf8_lossy(&sb);
    assert!(
        s.contains("a"),
        "earliest line should be in scrollback: {s:?}"
    );
}

#[test]
fn csi_unrecognized_is_noop_not_panic() {
    // ESC[?9999h is a private-mode set the model doesn't recognize.
    // Must not panic, and subsequent prints must still land.
    let mut m = TerminalModel::new(20, 3, 100);
    m.feed(b"\x1b[?9999h");
    m.feed(b"hi");
    let snap = m.snapshot_vt(20, 3);
    assert!(String::from_utf8_lossy(&snap).contains("hi"));
}

#[test]
fn alternate_screen_is_noop() {
    // DECSET 1049 (alternate screen) is intentionally a noop in PR-2.
    // After entering and leaving alt-screen, the main grid content must
    // still reflect everything written, including bytes "between" the
    // h/l pair (which a real implementation would have stashed away).
    let mut m = TerminalModel::new(20, 3, 100);
    m.feed(b"main");
    m.feed(b"\x1b[?1049halt");
    m.feed(b"\x1b[?1049l");
    let snap = m.snapshot_vt(20, 3);
    let s = String::from_utf8_lossy(&snap);
    assert!(s.contains("main"), "snapshot missing 'main': {s:?}");
    // EXPERIMENTAL: this is the bleed-through we accept in PR-2.
    assert!(
        s.contains("alt"),
        "alt-screen text should leak into main grid in PR-2: {s:?}"
    );
}

#[test]
fn decset_1004_tracks_focus_event_reporting_without_rev_bump() {
    // DECSET 1004 (focus event reporting) is a mode flag the daemon reads
    // to gate the synthetic mid-session OSC 10/11 theme write. It must be
    // tracked but, like alt-screen, MUST NOT bump the render rev (no
    // visible content change).
    let mut m = TerminalModel::new(20, 3, 100);
    assert!(
        !m.focus_event_tracking(),
        "1004 must start disabled (a fresh terminal hasn't opted in)"
    );
    let r0 = m.rev();
    m.feed(b"\x1b[?1004h");
    assert!(m.focus_event_tracking(), "CSI ?1004h must enable tracking");
    assert_eq!(m.rev(), r0, "enabling 1004 must not bump rev");
    m.feed(b"\x1b[?1004l");
    assert!(
        !m.focus_event_tracking(),
        "CSI ?1004l must disable tracking"
    );
    assert_eq!(m.rev(), r0, "disabling 1004 must not bump rev");
}

#[test]
fn cup_then_print_lands_at_target() {
    let mut m = TerminalModel::new(20, 5, 100);
    m.feed(b"\x1b[3;5HX");
    // After CUP 3;5 (1-indexed) → (2,4), print 'X', cursor advances to (2,5).
    let cur = m.cursor();
    assert_eq!(cur, Cursor { row: 2, col: 5 });
    let snap = m.snapshot_vt(20, 5);
    assert!(String::from_utf8_lossy(&snap).contains('X'));
}

#[test]
fn cr_lf_pair_resets_col_and_advances_row() {
    let mut m = TerminalModel::new(20, 5, 100);
    m.feed(b"abc\r\nxyz");
    // After CR LF, cursor at start of row 1. "xyz" printed on row 1.
    let snap = m.snapshot_vt(20, 5);
    let s = String::from_utf8_lossy(&snap);
    assert!(s.contains("abc"));
    assert!(s.contains("xyz"));
}

#[test]
fn ed_clears_screen() {
    let mut m = TerminalModel::new(20, 3, 100);
    m.feed(b"junk\x1b[2J");
    // After ED 2 (clear screen), the snapshot's visible area should be
    // blank. The state machine does emit "junk" first then the clear,
    // so we expect the snapshot's per-row content to be empty.
    let snap = m.snapshot_vt(20, 3);
    let s = String::from_utf8_lossy(&snap);
    // Should NOT contain "junk" because ED 2 clears everything.
    assert!(
        !s.contains("junk"),
        "ED 2 should have wiped 'junk', snapshot: {s:?}"
    );
}
