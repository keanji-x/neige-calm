//! State-mutation tests for `impl TerminalHandler for TerminalModel`, calling the trait directly (bypassing `vte::Parser`).

use calm_session::terminal_model::{
    Cursor, EraseMode, ScrollbackLimit, TerminalHandler, TerminalModel,
};

#[test]
fn print_then_line_feed_lands_text_on_next_row() {
    let mut m = TerminalModel::new(20, 5, 100);
    m.print('h');
    m.print('i');
    m.line_feed();
    assert_eq!(m.cursor(), Cursor { row: 1, col: 2 });

    let snap = m.snapshot_vt(20, 5);
    let s = String::from_utf8_lossy(&snap);
    assert!(s.contains("hi"), "snapshot missing 'hi': {s:?}");
}

#[test]
fn cursor_to_clamps_into_grid_bounds() {
    let mut m = TerminalModel::new(10, 3, 100);
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
    // The serializer may emit `;31` or `31;` — both legal; only check the param is present.
    assert!(s.contains("31"), "snapshot missing red SGR: {s:?}");
    assert!(s.contains('R'), "snapshot missing 'R': {s:?}");
}

#[test]
fn set_cursor_visible_toggles_snapshot_hide_show() {
    let mut m = TerminalModel::new(5, 1, 100);
    m.set_cursor_visible(false);
    let snap = m.snapshot_vt(5, 1);
    // The serializer always starts with `?25l` (hide while painting); only the trailing sequence reflects model state.
    let tail_start = snap.len() - 6;
    assert_eq!(&snap[tail_start..], b"\x1b[?25l");
}

#[test]
fn scroll_up_inner_evicts_top_row_to_scrollback() {
    let mut m = TerminalModel::new(10, 2, 100);
    m.print('a');
    m.line_feed();
    m.print('b');
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

#[test]
fn split_csi_across_feeds_parses_as_single_action() {
    // `TerminalModel::feed` takes the parser out and puts it back; a CSI straddling two PTY chunks must still resolve to a single trait call.
    let mut m = TerminalModel::new(20, 5, 100);
    m.feed(b"\x1b[");
    assert_eq!(
        m.cursor(),
        Cursor { row: 0, col: 0 },
        "partial CSI must not produce any handler call",
    );
    m.feed(b"5;10H");
    assert_eq!(
        m.cursor(),
        Cursor { row: 4, col: 9 },
        "CSI split across two feeds must resolve to a single CUP",
    );
}

/// Parse `rgb:RRRR/GGGG/BBBB` (xterm's 16-bit form) back into a `(r, g, b)` u8 triple.
fn parse_xterm_rgb_reply(reply: &[u8]) -> Option<(u8, u8, u8)> {
    let s = std::str::from_utf8(reply).ok()?;
    let semi = s.find(';')?;
    let after = &s[semi + 1..];
    let st = after.find('\x1b')?;
    let payload = &after[..st]; // "rgb:RRRR/GGGG/BBBB"
    let payload = payload.strip_prefix("rgb:")?;
    let parts: Vec<&str> = payload.split('/').collect();
    if parts.len() != 3 {
        return None;
    }
    let to_u8 = |hex: &str| -> Option<u8> {
        // High byte == original u8, since the emitter does `c * 257`.
        let v = u16::from_str_radix(hex, 16).ok()?;
        Some((v >> 8) as u8)
    };
    Some((to_u8(parts[0])?, to_u8(parts[1])?, to_u8(parts[2])?))
}

#[test]
fn osc_11_query_yields_reply_with_configured_bg() {
    let mut m = TerminalModel::with_colors(80, 24, 100, None, Some((17, 20, 24)));
    m.feed(b"\x1b]11;?\x1b\\");
    let reply = m.take_pending_osc_replies();
    assert!(
        reply.starts_with(b"\x1b]11;rgb:"),
        "expected OSC 11 reply prefix, got {reply:?}",
    );
    assert_eq!(parse_xterm_rgb_reply(&reply), Some((17, 20, 24)));
}

#[test]
fn osc_10_query_yields_reply_with_configured_fg() {
    let mut m = TerminalModel::with_colors(80, 24, 100, Some((216, 219, 226)), Some((17, 20, 24)));
    m.feed(b"\x1b]10;?\x1b\\");
    let reply = m.take_pending_osc_replies();
    assert!(
        reply.starts_with(b"\x1b]10;rgb:"),
        "expected OSC 10 reply prefix, got {reply:?}",
    );
    assert_eq!(parse_xterm_rgb_reply(&reply), Some((216, 219, 226)));
}

#[test]
fn osc_11_query_without_configured_bg_stays_silent() {
    // No default_bg configured → no reply; the child falls back to its built-in default.
    let mut m = TerminalModel::new(80, 24, 100);
    m.feed(b"\x1b]11;?\x1b\\");
    assert!(m.take_pending_osc_replies().is_empty());
}

#[test]
fn set_default_colors_updates_subsequent_query_reply() {
    let mut m = TerminalModel::with_colors(80, 24, 100, None, Some((17, 20, 24)));
    m.feed(b"\x1b]11;?\x1b\\");
    let first = m.take_pending_osc_replies();
    assert_eq!(parse_xterm_rgb_reply(&first), Some((17, 20, 24)));

    m.set_default_colors(None, Some((252, 254, 255)));
    m.feed(b"\x1b]11;?\x1b\\");
    let second = m.take_pending_osc_replies();
    assert_eq!(parse_xterm_rgb_reply(&second), Some((252, 254, 255)));
}

#[test]
fn take_pending_osc_replies_drains() {
    let mut m = TerminalModel::with_colors(80, 24, 100, None, Some((1, 2, 3)));
    m.feed(b"\x1b]11;?\x1b\\");
    assert!(!m.take_pending_osc_replies().is_empty());
    assert!(m.take_pending_osc_replies().is_empty());
}

#[test]
fn dsr_6n_yields_cursor_position_reply() {
    let mut m = TerminalModel::new(80, 24, 100);
    m.feed(b"\x1b[6n");
    let reply = m.take_pending_osc_replies();
    assert_eq!(
        reply, b"\x1b[1;1R",
        "expected DSR reply at initial cursor 1;1, got {reply:?}",
    );

    m.feed(b"\x1b[5;10H\x1b[6n");
    let reply = m.take_pending_osc_replies();
    assert_eq!(
        reply, b"\x1b[5;10R",
        "expected DSR reply at 5;10, got {reply:?}",
    );
}

#[test]
fn kitty_keyboard_query_yields_zero_u() {
    let mut m = TerminalModel::new(80, 24, 100);
    m.feed(b"\x1b[?u");
    let reply = m.take_pending_osc_replies();
    assert_eq!(
        reply, b"\x1b[?0u",
        "expected kitty-keyboard reply ?0u, got {reply:?}",
    );
}

#[test]
fn da1_query_yields_vt101_reply() {
    let mut m = TerminalModel::new(80, 24, 100);
    m.feed(b"\x1b[c");
    let reply = m.take_pending_osc_replies();
    assert_eq!(reply, b"\x1b[?1;0c", "expected DA1 reply, got {reply:?}",);

    m.feed(b"\x1b[0c");
    let reply = m.take_pending_osc_replies();
    assert_eq!(
        reply, b"\x1b[?1;0c",
        "expected DA1 reply for `CSI 0 c`, got {reply:?}",
    );
}

#[test]
fn split_sgr_across_feeds_applies_combined_attrs() {
    // SGR is also a CSI sequence, so the same split-across-feeds contract must hold; the serializer may emit `1;31` or `31;1`.
    let mut m = TerminalModel::new(10, 1, 100);
    m.feed(b"\x1b[1;");
    m.feed(b"31m");
    m.feed(b"R");
    let snap = m.snapshot_vt(10, 1);
    let s = String::from_utf8_lossy(&snap);
    assert!(s.contains("31"), "snapshot missing red SGR: {s:?}");
    assert!(s.contains('R'), "snapshot missing 'R': {s:?}");
}
