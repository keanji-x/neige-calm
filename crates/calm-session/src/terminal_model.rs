//! Server-side terminal model: vte-driven grid + scrollback + snapshot
//! serialization.
//!
//! Pure IO-free types. No tokio, no `Arc`/`Mutex`. The render plane in
//! [`crate::terminal_session::RenderPlane`] owns one [`TerminalModel`]; the
//! daemon shell feeds it raw PTY bytes via [`TerminalModel::feed`].
//!
//! ## Architecture (#69)
//!
//! The VT parser and the terminal state are split by a [`TerminalHandler`]
//! trait. [`VteProcessor`] owns a `vte::Parser` and translates each
//! `vte::Perform` callback into a `TerminalHandler` method call by VT
//! semantics (e.g. `print` / `cursor_to` / `erase_screen`). The grid /
//! cursor / scrollback / SGR mutation lives entirely in
//! `impl TerminalHandler for TerminalModel` — no parsing happens there,
//! no state mutation happens in `VteProcessor`.
//!
//! Reference tests use real terminal recordings (planned). Methodology
//! inspired by Warp/Alacritty's `Handler`-style separation between VT
//! parsing and grid mutation; Neige's implementation is original — no
//! AGPL code reuse.
//!
//! ## Pipeline
//!
//! 1. PTY chunk arrives → `RenderPlane::on_pty_chunk(bytes)`.
//! 2. `feed(bytes)` runs a [`VteProcessor`] over `&mut self` (since
//!    `TerminalModel` implements [`TerminalHandler`]); each visible state
//!    change bumps `rev` once.
//! 3. The raw bytes are simultaneously broadcast as a `RenderPatch` with
//!    `encoding = Vt` so xterm.js on the client gets the same bytes the
//!    server's model just consumed.
//! 4. On `ClientHello`, the render plane calls
//!    [`TerminalModel::snapshot_vt`] with the client's *desired* geometry
//!    (cols/rows). The result is a fresh ANSI byte stream that, when fed
//!    into an empty xterm, reproduces the current visible state — bound
//!    to the client's geometry, not the daemon's internal one.
//! 5. If the client asked for scrollback, the plane also calls
//!    [`TerminalModel::scrollback_vt`] and stuffs the result into
//!    `RenderSnapshot.scrollback`.
//!
//! ## Coverage (and what's NOT covered)
//!
//! Implemented well enough for bash/zsh/codex/claude TUI:
//! - CSI cursor moves: CUU/CUD/CUF/CUB/CUP/HVP
//! - CSI erase: ED (J), EL (K) — all variants 0/1/2
//! - CSI scroll: SU (S), SD (T)
//! - CSI SGR (m): full attribute set including 256-color and truecolor
//! - DECSET/DECRST 25 (cursor visibility — tracked but not emitted to wire)
//! - C0 controls: BS, HT, LF, CR, BEL
//!
//! **EXPERIMENTAL / first-pass only — known gaps**:
//! - **Alternate screen (DECSET 1049)** — `noop`. vim / less / htop will
//!   leak their alt-screen content into the main grid. Tracked as a
//!   follow-up; the snapshot will look weird until then.
//! - **Mouse / bracketed paste / focus events** — all ignored.
//! - **OSC** — OSC 10/11 color queries (`ESC ] N ; ? ESC \`) are answered
//!   when default fg/bg are configured (used to follow the host page theme
//!   — issue #177). Other OSC sequences (title, hyperlink, OSC 12 cursor
//!   color, etc.) remain ignored.
//! - **Sixel / kitty graphics** — ignored.
//! - **Wide characters (CJK, emoji)** — treated as single-width. Lines
//!   with wide chars may render at the wrong width on snapshot.
//! - **Combining characters** — overwrite the previous cell instead of
//!   combining. Visible artifacts on RTL / Hindi / Arabic.
//! - **DEC line drawing / G0/G1 character sets** — ignored.
//! - **Tab stops** — fixed at every 8 columns (no DECSC / TBC).
//!
//! 80% correctness against typical shells is the bar; the gap above is
//! the 20% we explicitly accept in this PR.

use std::collections::VecDeque;

use vte::{Params, Parser, Perform};

/// Scrollback limit honored by [`TerminalModel::scrollback_vt`] and by the
/// snapshot caller when deciding whether to populate
/// `RenderSnapshot.scrollback`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollbackLimit {
    /// No scrollback emitted.
    None,
    /// Every line the model still has buffered.
    All,
    /// Up to `n` most-recent scrolled-off lines.
    Lines(u32),
}

/// 0-indexed cursor position into the grid.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cursor {
    pub row: u16,
    pub col: u16,
}

/// Colors. `Default` means "the terminal's default fg/bg"; emitting it as
/// SGR is `39`/`49`. `Indexed(0..=15)` map to standard ANSI 30-37 / 90-97
/// (fg) and 40-47 / 100-107 (bg). Higher indices use SGR 38;5;n / 48;5;n.
/// Truecolor uses SGR 38;2;r;g;b / 48;2;r;g;b.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// SGR (Select Graphic Rendition) state. Cleared by `ESC[0m`; individual
/// attributes flipped by their respective SGR codes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SgrState {
    pub fg: Color,
    pub bg: Color,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub reverse: bool,
    pub hidden: bool,
    pub strikethrough: bool,
}

impl SgrState {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Serialize this state as the minimal SGR sequence that would set it
    /// from a clean (reset) state. Always starts with a `0;` reset to
    /// avoid inheriting attributes from whatever preceded.
    pub fn to_sgr_bytes(self) -> Vec<u8> {
        let mut params: Vec<String> = vec!["0".to_string()];
        if self.bold {
            params.push("1".into());
        }
        if self.dim {
            params.push("2".into());
        }
        if self.italic {
            params.push("3".into());
        }
        if self.underline {
            params.push("4".into());
        }
        if self.reverse {
            params.push("7".into());
        }
        if self.hidden {
            params.push("8".into());
        }
        if self.strikethrough {
            params.push("9".into());
        }
        match self.fg {
            Color::Default => {}
            Color::Indexed(i) if i < 8 => params.push((30 + i).to_string()),
            Color::Indexed(i) if (8..16).contains(&i) => params.push((90 + (i - 8)).to_string()),
            Color::Indexed(i) => params.push(format!("38;5;{i}")),
            Color::Rgb(r, g, b) => params.push(format!("38;2;{r};{g};{b}")),
        }
        match self.bg {
            Color::Default => {}
            Color::Indexed(i) if i < 8 => params.push((40 + i).to_string()),
            Color::Indexed(i) if (8..16).contains(&i) => params.push((100 + (i - 8)).to_string()),
            Color::Indexed(i) => params.push(format!("48;5;{i}")),
            Color::Rgb(r, g, b) => params.push(format!("48;2;{r};{g};{b}")),
        }
        format!("\x1b[{}m", params.join(";")).into_bytes()
    }
}

/// One cell in the grid: the printable character (single-width assumed)
/// plus its SGR attributes at the time of write. `' '` with default SGR
/// is the canonical "blank cell".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub sgr: SgrState,
}

impl Default for Cell {
    fn default() -> Self {
        Self {
            ch: ' ',
            sgr: SgrState::default(),
        }
    }
}

impl Cell {
    fn is_blank(&self) -> bool {
        self.ch == ' ' && self.sgr == SgrState::default()
    }
}

/// Self-built grid. `rows[i]` always has exactly `cols` entries.
#[derive(Debug, Clone)]
pub struct Grid {
    rows: Vec<Vec<Cell>>,
    cols: u16,
    rows_count: u16,
}

impl Grid {
    fn new(cols: u16, rows: u16) -> Self {
        let cols_usize = cols.max(1) as usize;
        let rows_usize = rows.max(1) as usize;
        Self {
            rows: vec![vec![Cell::default(); cols_usize]; rows_usize],
            cols: cols.max(1),
            rows_count: rows.max(1),
        }
    }

    fn cell(&self, row: u16, col: u16) -> Cell {
        self.rows
            .get(row as usize)
            .and_then(|r| r.get(col as usize).copied())
            .unwrap_or_default()
    }

    fn set_cell(&mut self, row: u16, col: u16, cell: Cell) {
        if let Some(r) = self.rows.get_mut(row as usize)
            && let Some(c) = r.get_mut(col as usize)
        {
            *c = cell;
        }
    }

    fn clear_row(&mut self, row: u16) {
        if let Some(r) = self.rows.get_mut(row as usize) {
            for c in r.iter_mut() {
                *c = Cell::default();
            }
        }
    }

    fn clear_row_from(&mut self, row: u16, from_col: u16) {
        if let Some(r) = self.rows.get_mut(row as usize) {
            for c in r.iter_mut().skip(from_col as usize) {
                *c = Cell::default();
            }
        }
    }

    fn clear_row_to(&mut self, row: u16, to_col_inclusive: u16) {
        if let Some(r) = self.rows.get_mut(row as usize) {
            let end = (to_col_inclusive as usize + 1).min(r.len());
            for c in r.iter_mut().take(end) {
                *c = Cell::default();
            }
        }
    }

    fn clear_all(&mut self) {
        for r in self.rows.iter_mut() {
            for c in r.iter_mut() {
                *c = Cell::default();
            }
        }
    }
}

/// Erase region selector for [`TerminalHandler::erase_screen`] /
/// [`TerminalHandler::erase_line`]. Matches xterm's CSI J / CSI K modes
/// (0 / 1 / 2) but named for clarity.
///
/// VT note: CSI 3 J ("also clear scrollback") is folded into [`Self::All`]
/// — we don't expose a separate variant because the current
/// implementation doesn't distinguish it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EraseMode {
    /// From the cursor to the end of the line / screen (CSI 0 J / 0 K).
    ToEnd,
    /// From the start of the line / screen to the cursor (CSI 1 J / 1 K).
    ToStart,
    /// Entire line / screen (CSI 2 J / 2 K, and CSI 3 J).
    All,
}

/// Trait implemented by the terminal state (grid + cursor + SGR +
/// scrollback) consumed by [`VteProcessor`].
///
/// Methods are named after VT semantics rather than byte codes; the
/// processor adapts each `vte::Perform` callback into the appropriate
/// trait method, so SGR parsing, CUP decoding, etc. live exactly once on
/// the implementation side.
///
/// Reference tests use real terminal recordings (planned). Methodology
/// inspired by Warp/Alacritty `Handler` separation; no AGPL code reuse.
pub trait TerminalHandler {
    /// Print one printable character at the current cursor position.
    /// Wide characters / combining marks are treated as single-width —
    /// see module doc "EXPERIMENTAL" notes.
    fn print(&mut self, c: char);

    // ---- C0 controls ------------------------------------------------

    /// CR (0x0D) — move cursor to column 0 of the current row.
    fn carriage_return(&mut self);
    /// LF / VT / FF (0x0A..=0x0C) — move cursor down one row, scrolling
    /// the top line into scrollback if past the bottom.
    fn line_feed(&mut self);
    /// BS (0x08) — move cursor one column left (no wrap to previous
    /// row).
    fn backspace(&mut self);
    /// HT (0x09) — advance cursor to the next 8-column tab stop, clamped
    /// to `cols - 1`.
    fn horizontal_tab(&mut self);
    /// BEL (0x07) — noop in this implementation.
    fn bell(&mut self);

    // ---- CSI cursor moves -------------------------------------------

    /// CUU (CSI A) — cursor up by `n`, saturating at row 0.
    fn cursor_up(&mut self, n: u16);
    /// CUD (CSI B / CSI e) — cursor down by `n`, clamped to last row.
    fn cursor_down(&mut self, n: u16);
    /// CUF (CSI C / CSI a) — cursor forward (right) by `n`, clamped to
    /// last col.
    fn cursor_forward(&mut self, n: u16);
    /// CUB (CSI D) — cursor back (left) by `n`, saturating at col 0.
    fn cursor_backward(&mut self, n: u16);

    /// CUP / HVP (CSI H / CSI f) — absolute cursor position. `row` /
    /// `col` are 0-indexed (the parser has already converted from the
    /// 1-indexed wire form). Both axes clamp into grid bounds.
    fn cursor_to(&mut self, row: u16, col: u16);

    /// CHA / HPA (CSI G / CSI \`) — absolute column position, 0-indexed.
    fn cursor_column(&mut self, col: u16);

    /// VPA (CSI d) — absolute row position, 0-indexed.
    fn cursor_row(&mut self, row: u16);

    // ---- CSI erase --------------------------------------------------

    /// ED (CSI J) — erase in display, relative to the cursor.
    fn erase_screen(&mut self, mode: EraseMode);

    /// EL (CSI K) — erase in line, relative to the cursor.
    fn erase_line(&mut self, mode: EraseMode);

    // ---- CSI scroll -------------------------------------------------

    /// SU (CSI S) — scroll the viewport up by `n` lines; the top `n`
    /// rows move into scrollback (no scroll region).
    fn scroll_up(&mut self, n: u16);
    /// SD (CSI T) — scroll the viewport down by `n` lines; the bottom
    /// `n` rows are dropped, top `n` filled with blanks.
    fn scroll_down(&mut self, n: u16);

    // ---- SGR --------------------------------------------------------

    /// SGR (CSI m) — set graphic rendition. `params` is the
    /// already-flattened sequence of SGR codes (extended-color
    /// `38;5;n` / `38;2;r;g;b` arrive as consecutive elements; the
    /// implementation walks them).
    fn set_sgr(&mut self, params: &[u16]);

    // ---- DEC private modes -----------------------------------------

    /// DECTCEM (CSI ?25 h/l) — show or hide the cursor.
    fn set_cursor_visible(&mut self, visible: bool);

    /// DECSET 1049 — enter alternate screen. Currently a noop; the
    /// method exists so a future PR can fill it in without touching the
    /// parser/trait boundary again.
    ///
    /// Invariant: noop implementations MUST NOT bump the render rev — the
    /// current `TerminalModel` impl is a noop and produces no visible
    /// state change, so `rev()` must remain unchanged across this call.
    /// A future real implementation with its own alt-screen grid would
    /// bump rev only when the visible viewport actually changes.
    fn enter_alt_screen(&mut self);

    /// DECRST 1049 — exit alternate screen. Currently a noop.
    ///
    /// Same no-bump-rev invariant as [`Self::enter_alt_screen`].
    fn exit_alt_screen(&mut self);

    /// DECSET/DECRST 1004 — focus event reporting (`CSI ?1004 h/l`).
    /// `enabled = true` for `h` (the child opted in to receiving
    /// `ESC[I`/`ESC[O` focus-in/out events), `false` for `l`.
    ///
    /// We track this purely as a *capability signal*, not because we
    /// generate focus events from the model: the daemon reads it (via
    /// [`crate::terminal_session::RenderPlane::focus_event_tracking`]) to
    /// decide whether a child is a focus-aware TUI (codex opts in on
    /// startup) or a passive consumer (a shell's line editor sits in raw
    /// mode at the prompt but never enables 1004). Mirrors zellij's
    /// `focus_event_tracking` gate.
    ///
    /// Invariant: this is a mode flag, not visible content — like
    /// alt-screen it MUST NOT bump the render rev.
    fn set_focus_event_tracking(&mut self, enabled: bool);

    /// OSC 10 / OSC 11 color query — `ESC ] 10 ; ? ST` or
    /// `ESC ] 11 ; ? ST`. `slot` is `10` (default foreground) or `11`
    /// (default background); the handler decides whether to push a
    /// reply (`ESC ] slot ; rgb:RRRR/GGGG/BBBB ST`) into its
    /// pending-write buffer.
    ///
    /// Default impl is a noop so non-`TerminalModel` test handlers
    /// don't have to implement it. The real impl on `TerminalModel`
    /// generates the OSC reply when default colors have been
    /// configured via [`TerminalModel::set_default_colors`].
    fn osc_color_query(&mut self, _slot: u8) {}

    /// DSR cursor position report (`CSI 6 n`) — the child probes for
    /// the current cursor position and expects `ESC [ row;col R` back
    /// (1-indexed wire format). codex's startup probe (#177) issues
    /// this alongside OSC 10/11 / CSI ?u / CSI c and waits for the
    /// reply before finalizing its terminal-capability cache; if we
    /// stay silent it burns its full 100ms timeout.
    ///
    /// Default impl is a noop. Real impl on `TerminalModel` pushes
    /// the reply into `pending_osc_replies`.
    fn device_status_report_cursor(&mut self) {}

    /// Kitty keyboard-enhancement query (`CSI ? u`) — the child asks
    /// what kitty keyboard-protocol flags this terminal supports. We
    /// support none, so the canonical reply is `ESC [ ? 0 u`. Same
    /// startup-probe story as DSR: silence forces codex to wait the
    /// full timeout.
    ///
    /// Default impl is a noop. Real impl on `TerminalModel` pushes
    /// the reply into `pending_osc_replies`.
    fn kitty_keyboard_query(&mut self) {}

    /// Primary device attributes (`CSI c` / `CSI 0 c`) — the child
    /// asks "what kind of terminal are you?". We answer with the
    /// minimum xterm-compatible DA1 string `ESC [ ? 1 ; 0 c`
    /// ("VT101, no options"), enough to satisfy codex's probe. DA2
    /// (`CSI > c`) and DA3 (`CSI = c`) are NOT handled here — keep
    /// the contract narrow.
    ///
    /// Default impl is a noop. Real impl on `TerminalModel` pushes
    /// the reply into `pending_osc_replies`.
    fn device_attributes_primary(&mut self) {}
}

/// VTE-to-handler adapter. Owns nothing of its own beyond a borrow of the
/// underlying [`TerminalHandler`]; implements `vte::Perform` and forwards
/// each callback to the appropriate trait method.
///
/// Never mutates grid / cursor / SGR state directly — all of that lives
/// in `impl TerminalHandler for TerminalModel`.
pub struct VteProcessor<'a, H: TerminalHandler + ?Sized> {
    handler: &'a mut H,
}

impl<'a, H: TerminalHandler + ?Sized> VteProcessor<'a, H> {
    pub fn new(handler: &'a mut H) -> Self {
        Self { handler }
    }

    fn first_param_or(params: &Params, default: u16) -> u16 {
        params
            .iter()
            .next()
            .and_then(|s| s.first().copied())
            .filter(|v| *v != 0)
            .unwrap_or(default)
    }

    fn first_param_raw(params: &Params) -> u16 {
        params
            .iter()
            .next()
            .and_then(|s| s.first().copied())
            .unwrap_or(0)
    }

    fn erase_mode_from(raw: u16) -> EraseMode {
        match raw {
            0 => EraseMode::ToEnd,
            1 => EraseMode::ToStart,
            // 2 — full; 3 — also scrollback, folded into All.
            _ => EraseMode::All,
        }
    }
}

impl<H: TerminalHandler + ?Sized> Perform for VteProcessor<'_, H> {
    fn print(&mut self, c: char) {
        self.handler.print(c);
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => self.handler.bell(),
            0x08 => self.handler.backspace(),
            0x09 => self.handler.horizontal_tab(),
            0x0a..=0x0c => self.handler.line_feed(),
            0x0d => self.handler.carriage_return(),
            _ => { /* other C0 controls noop */ }
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        // DEC private (ESC[?...) sequences arrive with intermediates =
        // b"?". We dispatch DECTCEM (25), DECSET 1049 (alt-screen) and
        // DECSET 1004 (focus event reporting); everything else is a noop.
        if intermediates == b"?" {
            match action {
                'h' => {
                    for s in params.iter() {
                        if let Some(&p) = s.first() {
                            match p {
                                25 => self.handler.set_cursor_visible(true),
                                1004 => self.handler.set_focus_event_tracking(true),
                                1049 => self.handler.enter_alt_screen(),
                                _ => { /* unknown DECSET: noop */ }
                            }
                        }
                    }
                }
                'l' => {
                    for s in params.iter() {
                        if let Some(&p) = s.first() {
                            match p {
                                25 => self.handler.set_cursor_visible(false),
                                1004 => self.handler.set_focus_event_tracking(false),
                                1049 => self.handler.exit_alt_screen(),
                                _ => { /* unknown DECRST: noop */ }
                            }
                        }
                    }
                }
                'u' => {
                    // Kitty keyboard-enhancement query (`CSI ? u`).
                    // We don't implement the kitty progressive
                    // protocol, so reply with flags=0. codex (#177)
                    // probes this at startup and blocks on the
                    // response.
                    self.handler.kitty_keyboard_query();
                }
                _ => { /* unknown ?-CSI: noop */ }
            }
            return;
        }

        // Vanilla CSI.
        match action {
            'A' => self.handler.cursor_up(Self::first_param_or(params, 1)),
            'B' | 'e' => self.handler.cursor_down(Self::first_param_or(params, 1)),
            'C' | 'a' => self.handler.cursor_forward(Self::first_param_or(params, 1)),
            'D' => self
                .handler
                .cursor_backward(Self::first_param_or(params, 1)),
            'H' | 'f' => {
                // CUP / HVP — wire is 1-indexed, trait API is 0-indexed.
                let mut it = params.iter();
                let row1 = it.next().and_then(|s| s.first().copied()).unwrap_or(1);
                let col1 = it.next().and_then(|s| s.first().copied()).unwrap_or(1);
                self.handler
                    .cursor_to(row1.saturating_sub(1), col1.saturating_sub(1));
            }
            'G' | '`' => {
                // CHA / HPA — 1-indexed col.
                let col1 = Self::first_param_or(params, 1);
                self.handler.cursor_column(col1.saturating_sub(1));
            }
            'd' => {
                // VPA — 1-indexed row.
                let row1 = Self::first_param_or(params, 1);
                self.handler.cursor_row(row1.saturating_sub(1));
            }
            'J' => self
                .handler
                .erase_screen(Self::erase_mode_from(Self::first_param_raw(params))),
            'K' => self
                .handler
                .erase_line(Self::erase_mode_from(Self::first_param_raw(params))),
            'S' => self.handler.scroll_up(Self::first_param_or(params, 1)),
            'T' => self.handler.scroll_down(Self::first_param_or(params, 1)),
            'm' => {
                // Flatten (semicolon + colon subparams) into a single
                // sequence; SGR walking lives in the handler so it sees
                // every code in order.
                if params.is_empty() {
                    self.handler.set_sgr(&[]);
                } else {
                    let flat: Vec<u16> = params.iter().flat_map(|s| s.iter().copied()).collect();
                    self.handler.set_sgr(&flat);
                }
            }
            // DSR — Device Status Report. `CSI 6 n` asks for the
            // cursor position; reply with `ESC [ row;col R`
            // (1-indexed). Other DSR params (5 = "status",
            // 25 = "DECSRC", ...) are ignored. Guard on empty
            // intermediates so we don't mishandle DEC-private
            // DSR variants like `CSI ? 6 n`.
            'n' if intermediates.is_empty() && Self::first_param_or(params, 0) == 6 => {
                self.handler.device_status_report_cursor();
            }
            // DA1 — Primary Device Attributes. `CSI c` (or
            // `CSI 0 c`) asks "what kind of terminal are you?".
            // DA2 (`CSI > c`) and DA3 (`CSI = c`) carry the same
            // final byte but live behind their own intermediates
            // — gate on empty intermediates so we only answer DA1.
            // Param defaults to 0 when omitted (per VT100 spec) so
            // both `CSI c` and `CSI 0 c` route here; non-zero params
            // fall through to the noop arm.
            'c' if intermediates.is_empty() && Self::first_param_or(params, 0) == 0 => {
                self.handler.device_attributes_primary();
            }
            // Unknown CSI: noop. NEVER panic — the protocol allows the
            // child to emit anything (mouse, bracketed paste, ...).
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {
        // ESC-only sequences (no CSI / OSC) — DECSC / DECRC / index / RI /
        // charset selection. All currently noop; flagged EXPERIMENTAL.
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        // OSC 10 (default fg) / OSC 11 (default bg) with `?` as the value
        // payload is a query — codex (and many TUIs) probes the terminal
        // for its theme this way at startup and on focus regains. Pass
        // it through to the handler so the model can push a reply into
        // its pending-write buffer; the daemon flushes those bytes to the
        // PTY master after `feed()`. Everything else stays noop (title,
        // hyperlinks, ...).
        let Some(first) = params.first() else { return };
        let Some(second) = params.get(1) else { return };
        if *second != b"?" {
            return;
        }
        let slot = match *first {
            b"10" => 10u8,
            b"11" => 11u8,
            // OSC 12 (cursor color) is deliberately silent — codex's
            // startup probe doesn't query it (verified via strace in
            // #177 P2). Adding a reply would be harmless but unneeded;
            // staying silent matches every other non-10/11 OSC slot.
            _ => return,
        };
        self.handler.osc_color_query(slot);
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
}

/// High-level driver: owns the [`vte::Parser`], the grid, cursor, SGR
/// state, and scrollback. Implements [`TerminalHandler`] so
/// [`VteProcessor`] can drive it directly. Exposes `feed` / `resize` /
/// `snapshot_vt` / `scrollback_vt` to the daemon.
pub struct TerminalModel {
    parser: Parser,
    grid: Grid,
    cursor: Cursor,
    sgr: SgrState,
    scrollback: VecDeque<Vec<Cell>>,
    scrollback_max_lines: usize,
    rev: u32,
    cursor_visible: bool,
    /// DECSET 1004 (focus event reporting) state. `true` once the child
    /// has sent `CSI ?1004 h`, cleared on `CSI ?1004 l`. The daemon reads
    /// this as a "focus-aware TUI" signal to gate the synthetic mid-session
    /// OSC 10/11 + focus-in write (see `daemon.rs`
    /// `Effect::TerminalThemeUpdate`) — a cooked-shell at the prompt (ZLE
    /// raw, no 1004) would otherwise echo color bytes into its line buffer
    /// (#295 / PR #296). Per-`TerminalModel`, so per-PTY/session and dies
    /// with the model; single writer (parser) + single reader (session
    /// loop) under the `render_plane` lock, no multi-client ambiguity.
    /// Not visible content, so it never bumps the render rev.
    focus_event_tracking: bool,
    /// Default foreground/background RGB the daemon advertises to the
    /// PTY child in reply to OSC 10/11 color queries. `None` means
    /// "stay silent" — the child falls back to its built-in default,
    /// which is what the daemon did historically (#177 first-fix).
    default_fg: Option<(u8, u8, u8)>,
    default_bg: Option<(u8, u8, u8)>,
    /// Bytes the daemon should push back onto the PTY master after the
    /// current `feed()` returns. Populated by [`Self::osc_color_query`]
    /// when a child probes OSC 10/11. The session loop drains via
    /// [`Self::take_pending_osc_replies`].
    pending_osc_replies: Vec<u8>,
}

impl TerminalModel {
    pub fn new(cols: u16, rows: u16, scrollback_max_lines: usize) -> Self {
        Self {
            parser: Parser::new(),
            grid: Grid::new(cols, rows),
            cursor: Cursor::default(),
            sgr: SgrState::default(),
            scrollback: VecDeque::new(),
            scrollback_max_lines,
            rev: 0,
            cursor_visible: true,
            focus_event_tracking: false,
            default_fg: None,
            default_bg: None,
            pending_osc_replies: Vec::new(),
        }
    }

    /// Same as [`Self::new`] but pre-seeds the default fg/bg the model
    /// will advertise on OSC 10/11 queries. Used by the daemon when the
    /// host browser has stamped its theme onto the CLI args so codex's
    /// startup probe gets an authoritative answer instead of falling
    /// back to its built-in default.
    pub fn with_colors(
        cols: u16,
        rows: u16,
        scrollback_max_lines: usize,
        default_fg: Option<(u8, u8, u8)>,
        default_bg: Option<(u8, u8, u8)>,
    ) -> Self {
        let mut m = Self::new(cols, rows, scrollback_max_lines);
        m.default_fg = default_fg;
        m.default_bg = default_bg;
        m
    }

    /// Replace the default fg/bg. The next OSC 10/11 query will reflect
    /// the new value. Pre-existing `pending_osc_replies` are not
    /// rewritten — they correspond to a query that already happened.
    pub fn set_default_colors(&mut self, fg: Option<(u8, u8, u8)>, bg: Option<(u8, u8, u8)>) {
        self.default_fg = fg;
        self.default_bg = bg;
    }

    pub fn default_fg(&self) -> Option<(u8, u8, u8)> {
        self.default_fg
    }

    pub fn default_bg(&self) -> Option<(u8, u8, u8)> {
        self.default_bg
    }

    /// Whether the child has enabled DECSET 1004 (focus event reporting).
    /// Read by the daemon to decide whether a child is a focus-aware TUI
    /// (codex opts in) vs. a passive consumer (a shell never does).
    pub fn focus_event_tracking(&self) -> bool {
        self.focus_event_tracking
    }

    /// Drain any OSC reply bytes the model produced since the last call.
    /// The daemon writes these to the PTY master after each `feed()` so
    /// the child reads its color-query answer on stdin via crossterm's
    /// event queue.
    pub fn take_pending_osc_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending_osc_replies)
    }

    fn bump(&mut self) {
        self.rev = self.rev.saturating_add(1);
    }

    fn clamp_cursor(&mut self) {
        if self.cursor.col >= self.grid.cols {
            self.cursor.col = self.grid.cols.saturating_sub(1);
        }
        if self.cursor.row >= self.grid.rows_count {
            self.cursor.row = self.grid.rows_count.saturating_sub(1);
        }
    }

    fn scroll_up_inner(&mut self, n: u16) {
        for _ in 0..n {
            if self.grid.rows.is_empty() {
                break;
            }
            let dropped = self.grid.rows.remove(0);
            self.scrollback.push_back(dropped);
            while self.scrollback.len() > self.scrollback_max_lines {
                self.scrollback.pop_front();
            }
            self.grid
                .rows
                .push(vec![Cell::default(); self.grid.cols as usize]);
        }
    }

    fn scroll_down_inner(&mut self, n: u16) {
        for _ in 0..n {
            if !self.grid.rows.is_empty() {
                self.grid.rows.pop();
            }
            self.grid
                .rows
                .insert(0, vec![Cell::default(); self.grid.cols as usize]);
        }
    }

    fn newline(&mut self) {
        // LF: cursor down one row; if past bottom, scroll up (eviction to
        // scrollback). xterm-by-default behaviour; we don't track the
        // scroll region (DECSTBM) — see EXPERIMENTAL notes in module doc.
        if self.cursor.row + 1 >= self.grid.rows_count {
            self.scroll_up_inner(1);
        } else {
            self.cursor.row += 1;
        }
    }

    /// Feed raw PTY bytes through the parser. Each visible state change
    /// bumps `rev()` by 1. Empty input bumps nothing.
    pub fn feed(&mut self, bytes: &[u8]) {
        // Take the parser out so we can hand `&mut self` to the
        // processor — the parser is logically separate from terminal
        // state and the borrow checker needs us to prove that.
        let mut parser = std::mem::replace(&mut self.parser, Parser::new());
        {
            let mut processor = VteProcessor::new(self);
            for &b in bytes {
                parser.advance(&mut processor, b);
            }
        }
        self.parser = parser;
    }

    /// Resize the internal grid. Existing content is clipped (cols
    /// reduced) or padded with blank cells (cols increased / rows
    /// increased). Rows reduced evict the *top* lines into scrollback
    /// (matches xterm). Cursor is clamped into the new geometry.
    /// Always bumps `rev` (caller expectation: any resize requires a
    /// fresh snapshot anyway).
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let new_cols = cols.max(1);
        let new_rows = rows.max(1);
        if new_cols == self.grid.cols && new_rows == self.grid.rows_count {
            // Identity resize: still bump so the daemon can decide to
            // re-broadcast a snapshot. Cheap, infrequent.
            self.bump();
            return;
        }

        // 1. Adjust per-row width.
        if new_cols != self.grid.cols {
            let new_cols_usize = new_cols as usize;
            for row in self.grid.rows.iter_mut() {
                row.resize(new_cols_usize, Cell::default());
            }
            self.grid.cols = new_cols;
        }

        // 2. Adjust row count.
        let cur_rows = self.grid.rows.len();
        let target_rows = new_rows as usize;
        match target_rows.cmp(&cur_rows) {
            std::cmp::Ordering::Greater => {
                // Pad below.
                let blank_row = vec![Cell::default(); new_cols as usize];
                for _ in cur_rows..target_rows {
                    self.grid.rows.push(blank_row.clone());
                }
            }
            std::cmp::Ordering::Less => {
                // Shrink rows. Two policies:
                //   - If the cursor survives bottom-drop (cursor_row <
                //     target_rows): discard bottom rows. Keeps the top
                //     content (typical "tiny shell at top" case).
                //   - Else: drop top rows into scrollback and shift the
                //     cursor up by that many rows (xterm-style anchor-to-
                //     bottom for an active prompt at/near bottom).
                let to_drop = cur_rows - target_rows;
                if (self.cursor.row as usize) < target_rows {
                    for _ in 0..to_drop {
                        self.grid.rows.pop();
                    }
                } else {
                    for _ in 0..to_drop {
                        if !self.grid.rows.is_empty() {
                            let dropped = self.grid.rows.remove(0);
                            self.scrollback.push_back(dropped);
                            while self.scrollback.len() > self.scrollback_max_lines {
                                self.scrollback.pop_front();
                            }
                        }
                    }
                    self.cursor.row = self.cursor.row.saturating_sub(to_drop as u16);
                }
            }
            std::cmp::Ordering::Equal => {}
        }
        self.grid.rows_count = new_rows;
        self.clamp_cursor();
        self.bump();
    }

    pub fn rev(&self) -> u32 {
        self.rev
    }

    pub fn size(&self) -> (u16, u16) {
        (self.grid.cols, self.grid.rows_count)
    }

    pub fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Serialize the current viewport at the requested geometry.
    ///
    /// Strategy: emit `ESC[?25l ESC[2J ESC[H`, then for each target row
    /// emit a cursor-position, an SGR reset, then per-cell (SGR-diff +
    /// char), then `ESC[K` to clear any trailing default cells the model
    /// has past the last non-blank cell. Finishes with cursor position +
    /// `ESC[?25h` (or `?25l` if the model says the cursor is hidden).
    ///
    /// If `target_cols/target_rows` differs from the internal grid we
    /// best-effort clip / pad — full geometry rebind (re-feeding the
    /// child's bytes at the new size) is out of scope; see module doc.
    pub fn snapshot_vt(&self, target_cols: u16, target_rows: u16) -> Vec<u8> {
        let mut out = Vec::with_capacity(target_cols as usize * target_rows as usize * 2);
        // 1. Hide cursor while painting; clear screen; home.
        out.extend_from_slice(b"\x1b[?25l\x1b[2J\x1b[H");

        let target_cols = target_cols.max(1);
        let target_rows = target_rows.max(1);

        for row_idx in 0..target_rows {
            // Position at row+1, col 1 (1-indexed).
            let pos = format!("\x1b[{};1H", row_idx + 1);
            out.extend_from_slice(pos.as_bytes());
            // Reset SGR — the row begins from a clean state.
            out.extend_from_slice(b"\x1b[0m");

            let mut last_sgr = SgrState::default();

            // Find last non-blank cell in this row so we don't emit a
            // trailing run of " " — `ESC[K` after the loop wipes the
            // remainder.
            let last_non_blank = {
                let mut found = None;
                for col_idx in 0..target_cols {
                    let cell = self.grid.cell(row_idx, col_idx);
                    if !cell.is_blank() {
                        found = Some(col_idx);
                    }
                }
                found
            };

            if let Some(end) = last_non_blank {
                for col_idx in 0..=end {
                    let cell = self.grid.cell(row_idx, col_idx);
                    if cell.sgr != last_sgr {
                        out.extend_from_slice(&cell.sgr.to_sgr_bytes());
                        last_sgr = cell.sgr;
                    }
                    push_char_utf8(&mut out, cell.ch);
                }
            }
            // Clear to end of line — covers cells past `end` and rows
            // past internal-grid cols when `target_cols > grid.cols`.
            out.extend_from_slice(b"\x1b[K");
        }

        // Reset SGR + position cursor + cursor visibility.
        out.extend_from_slice(b"\x1b[0m");
        let cur = self.cursor;
        // Clamp cursor into the target geometry.
        let row = (cur.row.min(target_rows.saturating_sub(1))) + 1;
        let col = (cur.col.min(target_cols.saturating_sub(1))) + 1;
        let pos = format!("\x1b[{};{}H", row, col);
        out.extend_from_slice(pos.as_bytes());
        if self.cursor_visible {
            out.extend_from_slice(b"\x1b[?25h");
        } else {
            out.extend_from_slice(b"\x1b[?25l");
        }
        out
    }

    /// Serialize the scrollback as ANSI bytes. The output is intended to
    /// be written into the client's terminal BEFORE `snapshot_vt` so the
    /// scrollback ends up in the client's own scrollback ring.
    ///
    /// Whole-line granularity: each scrollback row becomes one
    /// `\x1b[0m` reset + cell run + `\x1b[K\r\n`. Lines that exceed the
    /// requested `limit` are dropped from the *front* (oldest first).
    pub fn scrollback_vt(&self, limit: ScrollbackLimit) -> Vec<u8> {
        let max = match limit {
            ScrollbackLimit::None => return Vec::new(),
            ScrollbackLimit::All => self.scrollback.len(),
            ScrollbackLimit::Lines(n) => (n as usize).min(self.scrollback.len()),
        };
        if max == 0 {
            return Vec::new();
        }
        let start = self.scrollback.len() - max;
        let mut out = Vec::with_capacity(max * self.grid.cols as usize * 2);
        for line in self.scrollback.iter().skip(start) {
            out.extend_from_slice(b"\x1b[0m");
            let mut last_sgr = SgrState::default();
            // Strip trailing blanks for compactness.
            let mut end = 0usize;
            for (i, c) in line.iter().enumerate() {
                if !c.is_blank() {
                    end = i + 1;
                }
            }
            for cell in line.iter().take(end) {
                if cell.sgr != last_sgr {
                    out.extend_from_slice(&cell.sgr.to_sgr_bytes());
                    last_sgr = cell.sgr;
                }
                push_char_utf8(&mut out, cell.ch);
            }
            out.extend_from_slice(b"\x1b[K\r\n");
        }
        out
    }
}

// =========================================================================
// `TerminalHandler` impl: all grid/cursor/SGR/scrollback mutation lives
// here. `VteProcessor` calls into these methods; nothing in the parser
// adapter touches state directly. Every public method bumps `rev` once
// per visible state change (matches PR-2 semantics — see existing
// `terminal_model.rs` acceptance tests).
// =========================================================================
impl TerminalHandler for TerminalModel {
    fn print(&mut self, c: char) {
        // Wide-char and combining-char handling: see EXPERIMENTAL note.
        // Single-width assumed.
        let cell = Cell {
            ch: c,
            sgr: self.sgr,
        };
        self.clamp_cursor();
        self.grid.set_cell(self.cursor.row, self.cursor.col, cell);
        if self.cursor.col + 1 < self.grid.cols {
            self.cursor.col += 1;
        } else {
            // End of line: stay at the last column. xterm's "auto-wrap"
            // pending-wrap flag is intentionally simplified — the next
            // print will overwrite the last cell unless a CR/LF/CUP
            // arrives first. Sufficient for typical shell prompts.
            self.cursor.col = self.grid.cols.saturating_sub(1);
        }
        self.bump();
    }

    fn carriage_return(&mut self) {
        self.cursor.col = 0;
        self.bump();
    }

    fn line_feed(&mut self) {
        self.newline();
        self.bump();
    }

    fn backspace(&mut self) {
        if self.cursor.col > 0 {
            self.cursor.col -= 1;
            self.bump();
        }
    }

    fn horizontal_tab(&mut self) {
        // HT: jump to next 8-col boundary.
        let next = (self.cursor.col / 8 + 1) * 8;
        let max = self.grid.cols.saturating_sub(1);
        self.cursor.col = next.min(max);
        self.bump();
    }

    fn bell(&mut self) {
        // BEL: noop — does not bump rev (no visible change).
    }

    fn cursor_up(&mut self, n: u16) {
        self.cursor.row = self.cursor.row.saturating_sub(n);
        self.bump();
    }

    fn cursor_down(&mut self, n: u16) {
        let new_row = self.cursor.row.saturating_add(n);
        self.cursor.row = new_row.min(self.grid.rows_count.saturating_sub(1));
        self.bump();
    }

    fn cursor_forward(&mut self, n: u16) {
        let new_col = self.cursor.col.saturating_add(n);
        self.cursor.col = new_col.min(self.grid.cols.saturating_sub(1));
        self.bump();
    }

    fn cursor_backward(&mut self, n: u16) {
        self.cursor.col = self.cursor.col.saturating_sub(n);
        self.bump();
    }

    fn cursor_to(&mut self, row: u16, col: u16) {
        self.cursor.row = row.min(self.grid.rows_count.saturating_sub(1));
        self.cursor.col = col.min(self.grid.cols.saturating_sub(1));
        self.bump();
    }

    fn cursor_column(&mut self, col: u16) {
        self.cursor.col = col.min(self.grid.cols.saturating_sub(1));
        self.bump();
    }

    fn cursor_row(&mut self, row: u16) {
        self.cursor.row = row.min(self.grid.rows_count.saturating_sub(1));
        self.bump();
    }

    fn erase_screen(&mut self, mode: EraseMode) {
        match mode {
            EraseMode::ToEnd => {
                self.grid.clear_row_from(self.cursor.row, self.cursor.col);
                for r in (self.cursor.row + 1)..self.grid.rows_count {
                    self.grid.clear_row(r);
                }
            }
            EraseMode::ToStart => {
                for r in 0..self.cursor.row {
                    self.grid.clear_row(r);
                }
                self.grid.clear_row_to(self.cursor.row, self.cursor.col);
            }
            EraseMode::All => self.grid.clear_all(),
        }
        self.bump();
    }

    fn erase_line(&mut self, mode: EraseMode) {
        match mode {
            EraseMode::ToEnd => self.grid.clear_row_from(self.cursor.row, self.cursor.col),
            EraseMode::ToStart => self.grid.clear_row_to(self.cursor.row, self.cursor.col),
            EraseMode::All => self.grid.clear_row(self.cursor.row),
        }
        self.bump();
    }

    fn scroll_up(&mut self, n: u16) {
        self.scroll_up_inner(n);
        self.bump();
    }

    fn scroll_down(&mut self, n: u16) {
        self.scroll_down_inner(n);
        self.bump();
    }

    fn set_sgr(&mut self, params: &[u16]) {
        if params.is_empty() {
            self.sgr.reset();
            self.bump();
            return;
        }
        // Walk by param-position. Extended color sequences
        // (38;5;n / 38;2;r;g;b — colon or semicolon separated) arrive
        // pre-flattened from `VteProcessor`.
        let mut i = 0;
        while i < params.len() {
            let p = params[i];
            match p {
                0 => self.sgr.reset(),
                1 => self.sgr.bold = true,
                2 => self.sgr.dim = true,
                3 => self.sgr.italic = true,
                4 => self.sgr.underline = true,
                7 => self.sgr.reverse = true,
                8 => self.sgr.hidden = true,
                9 => self.sgr.strikethrough = true,
                22 => {
                    self.sgr.bold = false;
                    self.sgr.dim = false;
                }
                23 => self.sgr.italic = false,
                24 => self.sgr.underline = false,
                27 => self.sgr.reverse = false,
                28 => self.sgr.hidden = false,
                29 => self.sgr.strikethrough = false,
                30..=37 => self.sgr.fg = Color::Indexed((p - 30) as u8),
                38 => {
                    // 38;5;n or 38;2;r;g;b
                    if let Some(&kind) = params.get(i + 1) {
                        if kind == 5
                            && let Some(&n) = params.get(i + 2)
                        {
                            self.sgr.fg = Color::Indexed((n & 0xFF) as u8);
                            i += 2;
                        } else if kind == 2
                            && let (Some(&r), Some(&g), Some(&b)) =
                                (params.get(i + 2), params.get(i + 3), params.get(i + 4))
                        {
                            self.sgr.fg = Color::Rgb(r as u8, g as u8, b as u8);
                            i += 4;
                        }
                    }
                }
                39 => self.sgr.fg = Color::Default,
                40..=47 => self.sgr.bg = Color::Indexed((p - 40) as u8),
                48 => {
                    if let Some(&kind) = params.get(i + 1) {
                        if kind == 5
                            && let Some(&n) = params.get(i + 2)
                        {
                            self.sgr.bg = Color::Indexed((n & 0xFF) as u8);
                            i += 2;
                        } else if kind == 2
                            && let (Some(&r), Some(&g), Some(&b)) =
                                (params.get(i + 2), params.get(i + 3), params.get(i + 4))
                        {
                            self.sgr.bg = Color::Rgb(r as u8, g as u8, b as u8);
                            i += 4;
                        }
                    }
                }
                49 => self.sgr.bg = Color::Default,
                90..=97 => self.sgr.fg = Color::Indexed(8 + (p - 90) as u8),
                100..=107 => self.sgr.bg = Color::Indexed(8 + (p - 100) as u8),
                _ => { /* unknown SGR param — noop */ }
            }
            i += 1;
        }
        self.bump();
    }

    fn set_cursor_visible(&mut self, visible: bool) {
        self.cursor_visible = visible;
        self.bump();
    }

    fn enter_alt_screen(&mut self) {
        // EXPERIMENTAL: alternate-screen is a noop in this implementation.
        // See module-level "EXPERIMENTAL" note — vim / less / htop will
        // bleed through into the main grid until a follow-up wires this.
    }

    fn exit_alt_screen(&mut self) {
        // See `enter_alt_screen` — symmetric noop.
    }

    fn set_focus_event_tracking(&mut self, enabled: bool) {
        // Pure mode flag — record it, do NOT bump rev (no visible state
        // change). The daemon reads it via
        // `RenderPlane::focus_event_tracking()` to gate the synthetic
        // mid-session OSC 10/11 theme write.
        self.focus_event_tracking = enabled;
    }

    fn device_status_report_cursor(&mut self) {
        // CSI 6 n reply — `ESC [ row;col R`, both 1-indexed on the
        // wire. Our internal `Cursor` is 0-indexed; convert with +1.
        // codex (#177) blocks on this during startup; missing reply
        // burns the full 100ms probe timeout.
        let row1 = self.cursor.row.saturating_add(1);
        let col1 = self.cursor.col.saturating_add(1);
        let reply = format!("\x1b[{};{}R", row1, col1);
        self.pending_osc_replies.extend_from_slice(reply.as_bytes());
    }

    fn kitty_keyboard_query(&mut self) {
        // CSI ? u reply — flags=0 means "no kitty keyboard-protocol
        // enhancements supported". The progressive-enhancement
        // protocol is documented at
        // https://sw.kovidgoyal.net/kitty/keyboard-protocol/ ; we
        // intentionally advertise nothing so the child falls back to
        // legacy keycoding (what neige-calm has always done).
        self.pending_osc_replies.extend_from_slice(b"\x1b[?0u");
    }

    fn device_attributes_primary(&mut self) {
        // DA1 reply — `ESC [ ? 1 ; 0 c` ("VT101, no options"). This
        // is the minimum xterm-compatible response and is enough to
        // satisfy codex's startup capability probe.
        self.pending_osc_replies.extend_from_slice(b"\x1b[?1;0c");
    }

    fn osc_color_query(&mut self, slot: u8) {
        // OSC 10 → default fg, OSC 11 → default bg. xterm replies with
        // the 16-bit form `rgb:RRRR/GGGG/BBBB`; we mirror that — each
        // 8-bit channel `c` becomes `c * 257` (== `(c<<8)|c`) and emits
        // four hex digits. Terminated with the canonical ST
        // (`ESC \`). `bell_terminated` queries (`BEL`-terminated)
        // technically exist too, but ST is universally accepted and
        // codex specifically uses the parser shape that handles either.
        let rgb = match slot {
            10 => self.default_fg,
            11 => self.default_bg,
            _ => return,
        };
        let Some((r, g, b)) = rgb else {
            // No color configured → stay silent. The child falls back to
            // its built-in default, matching pre-#177 behaviour.
            return;
        };
        let to16 = |c: u8| (c as u16) * 257;
        let reply = format!(
            "\x1b]{};rgb:{:04x}/{:04x}/{:04x}\x1b\\",
            slot,
            to16(r),
            to16(g),
            to16(b),
        );
        self.pending_osc_replies.extend_from_slice(reply.as_bytes());
    }
}

fn push_char_utf8(out: &mut Vec<u8>, c: char) {
    let mut buf = [0u8; 4];
    let s = c.encode_utf8(&mut buf);
    out.extend_from_slice(s.as_bytes());
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn empty_feed_no_rev_bump() {
        let mut m = TerminalModel::new(80, 24, 100);
        let r0 = m.rev();
        m.feed(b"");
        assert_eq!(m.rev(), r0);
    }

    #[test]
    fn print_bumps_rev() {
        let mut m = TerminalModel::new(80, 24, 100);
        let r0 = m.rev();
        m.feed(b"hi");
        assert!(m.rev() > r0);
    }

    #[test]
    fn cursor_position_via_cup() {
        let mut m = TerminalModel::new(80, 24, 100);
        m.feed(b"\x1b[5;10H");
        // CUP 5;10 → 0-indexed (4, 9).
        assert_eq!(m.cursor(), Cursor { row: 4, col: 9 });
    }

    #[test]
    fn sgr_reset_via_zero() {
        let mut m = TerminalModel::new(80, 24, 100);
        m.feed(b"\x1b[1;31m");
        assert!(m.sgr.bold);
        m.feed(b"\x1b[0m");
        assert!(!m.sgr.bold);
        assert_eq!(m.sgr.fg, Color::Default);
    }
}
