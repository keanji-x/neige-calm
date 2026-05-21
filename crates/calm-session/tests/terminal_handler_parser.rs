//! Parser-side contract tests for [`VteProcessor`] (#69).
//!
//! These verify that `vte::Perform` byte sequences are translated into the
//! correct [`TerminalHandler`] method calls — *without* touching any
//! grid/cursor state. A `MockHandler` records every call as a tagged enum;
//! we feed canonical byte sequences (CR/LF, CUP, ED, SGR, DECTCEM, ...)
//! and assert on the recorded call list.
//!
//! See `terminal_handler_model.rs` for state-mutation tests against the
//! real `TerminalModel` impl.

use calm_session::terminal_model::{EraseMode, TerminalHandler, VteProcessor};
use vte::Parser;

/// One recorded handler method call.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Print(char),
    CarriageReturn,
    LineFeed,
    Backspace,
    HorizontalTab,
    Bell,
    CursorUp(u16),
    CursorDown(u16),
    CursorForward(u16),
    CursorBackward(u16),
    CursorTo(u16, u16),
    CursorColumn(u16),
    CursorRow(u16),
    EraseScreen(EraseMode),
    EraseLine(EraseMode),
    ScrollUp(u16),
    ScrollDown(u16),
    SetSgr(Vec<u16>),
    SetCursorVisible(bool),
    EnterAltScreen,
    ExitAltScreen,
}

#[derive(Default)]
struct MockHandler {
    calls: Vec<Call>,
}

impl TerminalHandler for MockHandler {
    fn print(&mut self, c: char) {
        self.calls.push(Call::Print(c));
    }
    fn carriage_return(&mut self) {
        self.calls.push(Call::CarriageReturn);
    }
    fn line_feed(&mut self) {
        self.calls.push(Call::LineFeed);
    }
    fn backspace(&mut self) {
        self.calls.push(Call::Backspace);
    }
    fn horizontal_tab(&mut self) {
        self.calls.push(Call::HorizontalTab);
    }
    fn bell(&mut self) {
        self.calls.push(Call::Bell);
    }
    fn cursor_up(&mut self, n: u16) {
        self.calls.push(Call::CursorUp(n));
    }
    fn cursor_down(&mut self, n: u16) {
        self.calls.push(Call::CursorDown(n));
    }
    fn cursor_forward(&mut self, n: u16) {
        self.calls.push(Call::CursorForward(n));
    }
    fn cursor_backward(&mut self, n: u16) {
        self.calls.push(Call::CursorBackward(n));
    }
    fn cursor_to(&mut self, row: u16, col: u16) {
        self.calls.push(Call::CursorTo(row, col));
    }
    fn cursor_column(&mut self, col: u16) {
        self.calls.push(Call::CursorColumn(col));
    }
    fn cursor_row(&mut self, row: u16) {
        self.calls.push(Call::CursorRow(row));
    }
    fn erase_screen(&mut self, mode: EraseMode) {
        self.calls.push(Call::EraseScreen(mode));
    }
    fn erase_line(&mut self, mode: EraseMode) {
        self.calls.push(Call::EraseLine(mode));
    }
    fn scroll_up(&mut self, n: u16) {
        self.calls.push(Call::ScrollUp(n));
    }
    fn scroll_down(&mut self, n: u16) {
        self.calls.push(Call::ScrollDown(n));
    }
    fn set_sgr(&mut self, params: &[u16]) {
        self.calls.push(Call::SetSgr(params.to_vec()));
    }
    fn set_cursor_visible(&mut self, visible: bool) {
        self.calls.push(Call::SetCursorVisible(visible));
    }
    fn enter_alt_screen(&mut self) {
        self.calls.push(Call::EnterAltScreen);
    }
    fn exit_alt_screen(&mut self) {
        self.calls.push(Call::ExitAltScreen);
    }
}

/// Drive a fresh `vte::Parser` + `VteProcessor` over `bytes`, return the
/// recorded call list. One helper to keep the assertion sites tight.
fn drive(bytes: &[u8]) -> Vec<Call> {
    let mut mock = MockHandler::default();
    let mut parser = Parser::new();
    let mut proc = VteProcessor::new(&mut mock);
    for &b in bytes {
        parser.advance(&mut proc, b);
    }
    mock.calls
}

#[test]
fn print_single_char() {
    assert_eq!(drive(b"a"), vec![Call::Print('a')]);
}

#[test]
fn cr_lf_decomposes_into_two_calls() {
    assert_eq!(drive(b"\r\n"), vec![Call::CarriageReturn, Call::LineFeed]);
}

#[test]
fn c0_controls_route_to_named_methods() {
    // BS, HT, BEL — verify each routes to the right handler method.
    assert_eq!(drive(b"\x08"), vec![Call::Backspace]);
    assert_eq!(drive(b"\x09"), vec![Call::HorizontalTab]);
    assert_eq!(drive(b"\x07"), vec![Call::Bell]);
}

#[test]
fn ed_2_routes_to_erase_screen_all() {
    assert_eq!(drive(b"\x1b[2J"), vec![Call::EraseScreen(EraseMode::All)],);
}

#[test]
fn ed_modes_map_to_erase_mode_enum() {
    assert_eq!(drive(b"\x1b[0J"), vec![Call::EraseScreen(EraseMode::ToEnd)],);
    assert_eq!(
        drive(b"\x1b[1J"),
        vec![Call::EraseScreen(EraseMode::ToStart)],
    );
    // CSI J with no param is equivalent to CSI 0 J.
    assert_eq!(drive(b"\x1b[J"), vec![Call::EraseScreen(EraseMode::ToEnd)],);
}

#[test]
fn el_modes_map_to_erase_mode_enum() {
    assert_eq!(drive(b"\x1b[0K"), vec![Call::EraseLine(EraseMode::ToEnd)],);
    assert_eq!(drive(b"\x1b[1K"), vec![Call::EraseLine(EraseMode::ToStart)],);
    assert_eq!(drive(b"\x1b[2K"), vec![Call::EraseLine(EraseMode::All)],);
}

#[test]
fn cup_3_5_routes_to_cursor_to_with_zero_indexed_args() {
    // Wire is 1-indexed; trait API is 0-indexed.
    assert_eq!(drive(b"\x1b[3;5H"), vec![Call::CursorTo(2, 4)]);
}

#[test]
fn cup_defaults_to_1_1_when_omitted() {
    // CSI H with no params == CUP 1;1 → (0,0) at the trait API.
    assert_eq!(drive(b"\x1b[H"), vec![Call::CursorTo(0, 0)]);
}

#[test]
fn cursor_moves_default_to_one_when_param_omitted() {
    assert_eq!(drive(b"\x1b[A"), vec![Call::CursorUp(1)]);
    assert_eq!(drive(b"\x1b[B"), vec![Call::CursorDown(1)]);
    assert_eq!(drive(b"\x1b[C"), vec![Call::CursorForward(1)]);
    assert_eq!(drive(b"\x1b[D"), vec![Call::CursorBackward(1)]);
}

#[test]
fn cursor_moves_honor_explicit_param() {
    assert_eq!(drive(b"\x1b[5A"), vec![Call::CursorUp(5)]);
    assert_eq!(drive(b"\x1b[3B"), vec![Call::CursorDown(3)]);
    assert_eq!(drive(b"\x1b[7C"), vec![Call::CursorForward(7)]);
    assert_eq!(drive(b"\x1b[2D"), vec![Call::CursorBackward(2)]);
}

#[test]
fn cha_vpa_route_to_axis_specific_methods() {
    // CSI 10 G — CHA → cursor_column(9) after 1-indexed conversion.
    assert_eq!(drive(b"\x1b[10G"), vec![Call::CursorColumn(9)]);
    // CSI 7 d — VPA → cursor_row(6).
    assert_eq!(drive(b"\x1b[7d"), vec![Call::CursorRow(6)]);
}

#[test]
fn scroll_su_sd_route_with_default_one() {
    assert_eq!(drive(b"\x1b[S"), vec![Call::ScrollUp(1)]);
    assert_eq!(drive(b"\x1b[3T"), vec![Call::ScrollDown(3)]);
}

#[test]
fn sgr_bold_red_flattens_to_param_slice() {
    // Two params, semicolon separated → set_sgr([1, 31]).
    assert_eq!(drive(b"\x1b[1;31m"), vec![Call::SetSgr(vec![1, 31])]);
}

#[test]
fn sgr_with_no_params_arrives_as_zero() {
    // `vte` normalizes `CSI m` (no params) to a single 0 param —
    // semantically equivalent to `CSI 0 m`. Either form must therefore
    // reach the handler as `set_sgr([0])`.
    assert_eq!(drive(b"\x1b[m"), vec![Call::SetSgr(vec![0])]);
    assert_eq!(drive(b"\x1b[0m"), vec![Call::SetSgr(vec![0])]);
}

#[test]
fn sgr_256_color_flattens_subparams() {
    // 38;5;196 — flat sequence should arrive as one set_sgr call.
    assert_eq!(
        drive(b"\x1b[38;5;196m"),
        vec![Call::SetSgr(vec![38, 5, 196])],
    );
}

#[test]
fn dectcem_show_hide_routes_to_set_cursor_visible() {
    assert_eq!(drive(b"\x1b[?25l"), vec![Call::SetCursorVisible(false)],);
    assert_eq!(drive(b"\x1b[?25h"), vec![Call::SetCursorVisible(true)],);
}

#[test]
fn decset_1049_routes_to_enter_exit_alt_screen() {
    // Even though the impl is a noop, the parser MUST surface these so a
    // future PR can wire alt-screen without re-touching `VteProcessor`.
    assert_eq!(drive(b"\x1b[?1049h"), vec![Call::EnterAltScreen]);
    assert_eq!(drive(b"\x1b[?1049l"), vec![Call::ExitAltScreen]);
}

#[test]
fn unknown_csi_is_silent_noop() {
    // CSI ?9999 h — unknown DEC private. Must NOT produce any handler
    // call and must NOT panic.
    assert_eq!(drive(b"\x1b[?9999h"), vec![]);
    // Vanilla unknown final byte.
    assert_eq!(drive(b"\x1b[1;2Z"), vec![]);
}

#[test]
fn combined_sequence_text_then_clear_then_text() {
    // Realistic snippet: print "hi", CR LF, ED 2, CUP 1;1, print "x".
    assert_eq!(
        drive(b"hi\r\n\x1b[2J\x1b[1;1Hx"),
        vec![
            Call::Print('h'),
            Call::Print('i'),
            Call::CarriageReturn,
            Call::LineFeed,
            Call::EraseScreen(EraseMode::All),
            Call::CursorTo(0, 0),
            Call::Print('x'),
        ],
    );
}
