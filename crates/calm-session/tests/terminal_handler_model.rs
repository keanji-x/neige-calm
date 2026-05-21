//! State-mutation tests for `impl TerminalHandler for TerminalModel` (#69).
//!
//! These exercise the trait directly — bypassing `vte::Parser` — to prove
//! the handler methods on the real model mutate grid/cursor/SGR as
//! expected. Together with `terminal_handler_parser.rs` (parser-side) and
//! `terminal_model.rs` (full pipeline) this closes the loop on the
//! parser/state split.

use calm_session::terminal_model::{
    Cursor, EraseMode, ScrollbackLimit, TerminalHandler, TerminalModel,
};

#[test]
fn print_then_line_feed_lands_text_on_next_row() {
    // Direct trait calls — no escape bytes go through the parser.
    let mut m = TerminalModel::new(20, 5, 100);
    m.print('h');
    m.print('i');
    m.line_feed();
    // After LF, cursor.row advanced from 0 to 1; column unchanged at 2.
    assert_eq!(m.cursor(), Cursor { row: 1, col: 2 });

    let snap = m.snapshot_vt(20, 5);
    let s = String::from_utf8_lossy(&snap);
    assert!(s.contains("hi"), "snapshot missing 'hi': {s:?}");
}

#[test]
fn cursor_to_clamps_into_grid_bounds() {
    let mut m = TerminalModel::new(10, 3, 100);
    // Target way past the grid — must clamp to (rows-1, cols-1).
    m.cursor_to(99, 99);
    assert_eq!(m.cursor(), Cursor { row: 2, col: 9 });
}

#[test]
fn erase_screen_all_wipes_grid() {
    let mut m = TerminalModel::new(10, 3, 100);
    m.print('a');
    m.print('b');
    m.print('c');
    m.erase_screen(EraseMode::All);
    let snap = m.snapshot_vt(10, 3);
    let s = String::from_utf8_lossy(&snap);
    assert!(!s.contains("abc"), "ED All left 'abc': {s:?}");
}

#[test]
fn set_sgr_bold_red_then_print_carries_attrs() {
    let mut m = TerminalModel::new(10, 1, 100);
    m.set_sgr(&[1, 31]); // bold + red fg
    m.print('R');
    let snap = m.snapshot_vt(10, 1);
    let s = String::from_utf8_lossy(&snap);
    // The serializer emits ;31 (or 31;) for fg red — both legal SGR
    // composings; we just check the param is present.
    assert!(s.contains("31"), "snapshot missing red SGR: {s:?}");
    assert!(s.contains('R'), "snapshot missing 'R': {s:?}");
}

#[test]
fn set_cursor_visible_toggles_snapshot_hide_show() {
    let mut m = TerminalModel::new(5, 1, 100);
    m.set_cursor_visible(false);
    let snap = m.snapshot_vt(5, 1);
    // The snapshot serializer always starts with `?25l` (hide while
    // painting) and finishes with either `?25h` or `?25l` depending on
    // model state. Verify the trailing byte sequence is `?25l`.
    let tail_start = snap.len() - 6;
    assert_eq!(&snap[tail_start..], b"\x1b[?25l");
}

#[test]
fn scroll_up_inner_evicts_top_row_to_scrollback() {
    // 10x2 grid with two distinct rows. Scroll up by 1 → top row moves
    // into scrollback; bottom row shifts up; bottom becomes blank.
    let mut m = TerminalModel::new(10, 2, 100);
    m.print('a');
    m.line_feed();
    m.print('b');
    // Scroll the (then-)top row 'a' off into scrollback.
    m.scroll_up(1);

    let sb = m.scrollback_vt(ScrollbackLimit::All);
    let s = String::from_utf8_lossy(&sb);
    assert!(
        s.contains('a'),
        "expected 'a' in scrollback after scroll_up: {s:?}",
    );
}

#[test]
fn carriage_return_resets_column() {
    let mut m = TerminalModel::new(10, 2, 100);
    m.print('x');
    m.print('y');
    m.print('z');
    assert_eq!(m.cursor(), Cursor { row: 0, col: 3 });
    m.carriage_return();
    assert_eq!(m.cursor(), Cursor { row: 0, col: 0 });
}
