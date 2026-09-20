//! Parser-side contract tests for [`VteProcessor`]: byte sequences must translate into the correct
//! [`TerminalHandler`] calls, recorded by a `MockHandler` without touching any grid/cursor state.

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
    SetMouseMode(u16, bool),
    SetBracketedPaste(bool),
    SetFocusEventTracking(bool),
    OscColorQuery(u8),
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
    fn set_mouse_mode(&mut self, code: u16, enabled: bool) {
        self.calls.push(Call::SetMouseMode(code, enabled));
    }
    fn set_bracketed_paste(&mut self, enabled: bool) {
        self.calls.push(Call::SetBracketedPaste(enabled));
    }
    fn set_focus_event_tracking(&mut self, enabled: bool) {
        self.calls.push(Call::SetFocusEventTracking(enabled));
    }
    fn osc_color_query(&mut self, slot: u8) {
        self.calls.push(Call::OscColorQuery(slot));
    }
}

/// Drive a fresh `vte::Parser` + `VteProcessor` over `bytes`, return the recorded call list.
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
    assert_eq!(drive(b"\x1b[3;5H"), vec![Call::CursorTo(2, 4)]);
}

#[test]
fn cup_defaults_to_1_1_when_omitted() {
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
    assert_eq!(drive(b"\x1b[10G"), vec![Call::CursorColumn(9)]);
    assert_eq!(drive(b"\x1b[7d"), vec![Call::CursorRow(6)]);
}

#[test]
fn scroll_su_sd_route_with_default_one() {
    assert_eq!(drive(b"\x1b[S"), vec![Call::ScrollUp(1)]);
    assert_eq!(drive(b"\x1b[3T"), vec![Call::ScrollDown(3)]);
}

#[test]
fn sgr_bold_red_flattens_to_param_slice() {
    assert_eq!(drive(b"\x1b[1;31m"), vec![Call::SetSgr(vec![1, 31])]);
}

#[test]
fn sgr_with_no_params_arrives_as_zero() {
    // `vte` normalizes `CSI m` (no params) to a single 0 param.
    assert_eq!(drive(b"\x1b[m"), vec![Call::SetSgr(vec![0])]);
    assert_eq!(drive(b"\x1b[0m"), vec![Call::SetSgr(vec![0])]);
}

#[test]
fn sgr_256_color_flattens_subparams() {
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
    assert_eq!(drive(b"\x1b[?1049h"), vec![Call::EnterAltScreen]);
    assert_eq!(drive(b"\x1b[?1049l"), vec![Call::ExitAltScreen]);
}

#[test]
fn decset_mouse_and_paste_route_to_mode_setters() {
    assert_eq!(
        drive(b"\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?2004h"),
        vec![
            Call::SetMouseMode(1000, true),
            Call::SetMouseMode(1002, true),
            Call::SetMouseMode(1006, true),
            Call::SetBracketedPaste(true),
        ],
    );
    assert_eq!(
        drive(b"\x1b[?1000l\x1b[?2004l"),
        vec![
            Call::SetMouseMode(1000, false),
            Call::SetBracketedPaste(false),
        ],
    );
}

#[test]
fn decset_1004_routes_to_set_focus_event_tracking() {
    assert_eq!(
        drive(b"\x1b[?1004h"),
        vec![Call::SetFocusEventTracking(true)],
    );
    assert_eq!(
        drive(b"\x1b[?1004l"),
        vec![Call::SetFocusEventTracking(false)],
    );
}

#[test]
fn unknown_csi_is_silent_noop() {
    // Unknown DEC private: no handler call and no panic.
    assert_eq!(drive(b"\x1b[?9999h"), vec![]);
    assert_eq!(drive(b"\x1b[1;2Z"), vec![]);
}

#[test]
fn osc_11_query_routes_to_osc_color_query_slot_11() {
    assert_eq!(drive(b"\x1b]11;?\x1b\\"), vec![Call::OscColorQuery(11)]);
}

#[test]
fn osc_10_query_routes_to_osc_color_query_slot_10() {
    assert_eq!(drive(b"\x1b]10;?\x1b\\"), vec![Call::OscColorQuery(10)]);
}

#[test]
fn osc_11_with_rgb_payload_is_not_a_query() {
    // Only the literal `?` payload is a query; a reply-shaped OSC 11 must not surface.
    assert_eq!(drive(b"\x1b]11;rgb:0/0/0\x1b\\"), vec![]);
}

#[test]
fn osc_unrelated_slot_with_query_payload_is_silent() {
    assert_eq!(drive(b"\x1b]12;?\x1b\\"), vec![]);
}

#[test]
fn combined_sequence_text_then_clear_then_text() {
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
