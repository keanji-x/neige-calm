//! Acceptance tests for the server-side [`TerminalModel`]: grid + scrollback + snapshot serializer, no IO.

use calm_session::terminal_model::{Cursor, ScrollbackLimit, TerminalModel};

#[test]
fn model_feeds_basic_ansi() {
    let mut m = TerminalModel::new(20, 5, 100);
    m.feed(b"hello\x1b[2;1Hworld");
    let snap = m.snapshot_vt(20, 5);
    let s = String::from_utf8_lossy(&snap);
    assert!(s.contains("hello"), "snapshot missing 'hello': {s:?}");
    assert!(s.contains("world"), "snapshot missing 'world': {s:?}");
}

#[test]
fn model_resize_preserves_grid_within_bounds() {
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
fn model_narrow_mutation_then_widen_does_not_restore_stale_hidden_tail() {
    let mut m = TerminalModel::new(80, 4, 100);
    m.feed(b"prefix\x1b[1;61HTAIL-MARKER");

    m.resize(40, 4);
    m.feed(b"\rreplacement");

    m.resize(80, 4);
    let restored = m.snapshot_vt(80, 4);
    assert!(
        !String::from_utf8_lossy(&restored).contains("TAIL-MARKER"),
        "widening must not resurrect a stale suffix clipped at narrow width"
    );
    assert!(String::from_utf8_lossy(&restored).contains("replacement"));
}

#[test]
fn model_shorter_resize_keeps_bottom_output_and_scrolls_top() {
    let mut m = TerminalModel::new(40, 6, 100);
    m.feed(b"TOP-MARKER\x1b[6;1HBOTTOM-MARKER\x1b[1;1H");

    // The cursor is deliberately at the top; that must not be a reason to pop bottom rows.
    m.resize(40, 3);

    let viewport = m.snapshot_vt(40, 3);
    assert!(
        String::from_utf8_lossy(&viewport).contains("BOTTOM-MARKER"),
        "shrinking must preserve the newest/bottom output"
    );
    let scrollback = m.scrollback_vt(ScrollbackLimit::All);
    assert!(
        String::from_utf8_lossy(&scrollback).contains("TOP-MARKER"),
        "evicted top content should remain recoverable in scrollback"
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
    m.feed(b"\0");
    assert_eq!(m.rev(), r1, "noop byte (NUL) must not bump rev");
}

#[test]
fn sgr_state_tracks_csi() {
    let mut m = TerminalModel::new(10, 1, 100);
    m.feed(b"\x1b[31mred\x1b[0mx");
    let snap = m.snapshot_vt(10, 1);
    let s = String::from_utf8_lossy(&snap);
    assert!(s.contains("31m"), "snapshot missing 'red' SGR: {s:?}");
    assert!(s.contains("red"), "snapshot missing 'red' text: {s:?}");
    assert!(s.contains('x'), "snapshot missing trailing 'x'");
}

#[test]
fn scrollback_grows_on_lf_overflow() {
    let mut m = TerminalModel::new(10, 2, 100);
    m.feed(b"a\nb\nc\nd\ne\n");
    let sb = m.scrollback_vt(ScrollbackLimit::All);
    // The line count is hard to read from the byte stream; the earliest line "a" appearing proves it scrolled out.
    let s = String::from_utf8_lossy(&sb);
    assert!(
        s.contains("a"),
        "earliest line should be in scrollback: {s:?}"
    );
}

#[test]
fn csi_unrecognized_is_noop_not_panic() {
    let mut m = TerminalModel::new(20, 3, 100);
    m.feed(b"\x1b[?9999h");
    m.feed(b"hi");
    let snap = m.snapshot_vt(20, 3);
    assert!(String::from_utf8_lossy(&snap).contains("hi"));
}

#[test]
fn alternate_screen_is_noop() {
    // DECSET 1049 is intentionally only a flag: bytes written "between" the h/l pair land in the main grid.
    let mut m = TerminalModel::new(20, 3, 100);
    m.feed(b"main");
    m.feed(b"\x1b[?1049halt");
    m.feed(b"\x1b[?1049l");
    let snap = m.snapshot_vt(20, 3);
    let s = String::from_utf8_lossy(&snap);
    assert!(s.contains("main"), "snapshot missing 'main': {s:?}");
    assert!(
        s.contains("alt"),
        "alt-screen text should leak into main grid in PR-2: {s:?}"
    );
    assert!(
        !s.contains("\u{1b}[?1049h"),
        "1049l must clear the restore flag: {s:?}"
    );
}

#[test]
fn snapshot_vt_restores_mouse_and_alt_modes() {
    // A refreshed xterm.js starts blank; without the mode CSIs in the snapshot, wheel/clicks never reach the child.
    let mut m = TerminalModel::new(20, 3, 100);
    m.feed(b"\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?2004hhi");
    let bytes = m.snapshot_vt(20, 3);
    let snap = String::from_utf8_lossy(&bytes);
    for needle in [
        "\u{1b}[?1049h",
        "\u{1b}[?1000h",
        "\u{1b}[?1002h",
        "\u{1b}[?1006h",
        "\u{1b}[?2004h",
    ] {
        assert!(
            snap.contains(needle),
            "snapshot missing {needle:?}: {snap:?}"
        );
    }
    assert!(snap.contains("hi"), "snapshot missing grid text: {snap:?}");
    let prefix = snap.split("hi").next().unwrap_or("");
    let idx_1049 = prefix.find("\u{1b}[?1049h").expect("1049h");
    let idx_clear = prefix.find("\u{1b}[2J").expect("ED2");
    assert!(
        idx_1049 < idx_clear,
        "alt-screen restore must precede the screen clear: {snap:?}"
    );
}

#[test]
fn decset_1004_tracks_focus_event_reporting_without_rev_bump() {
    // DECSET 1004 is a mode flag: tracked but, like alt-screen, MUST NOT bump the render rev.
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
    let cur = m.cursor();
    assert_eq!(cur, Cursor { row: 2, col: 5 });
    let snap = m.snapshot_vt(20, 5);
    assert!(String::from_utf8_lossy(&snap).contains('X'));
}

#[test]
fn cr_lf_pair_resets_col_and_advances_row() {
    let mut m = TerminalModel::new(20, 5, 100);
    m.feed(b"abc\r\nxyz");
    let snap = m.snapshot_vt(20, 5);
    let s = String::from_utf8_lossy(&snap);
    assert!(s.contains("abc"));
    assert!(s.contains("xyz"));
}

#[test]
fn ed_clears_screen() {
    let mut m = TerminalModel::new(20, 3, 100);
    m.feed(b"junk\x1b[2J");
    let snap = m.snapshot_vt(20, 3);
    let s = String::from_utf8_lossy(&snap);
    assert!(
        !s.contains("junk"),
        "ED 2 should have wiped 'junk', snapshot: {s:?}"
    );
}
