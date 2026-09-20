//! Server-side terminal model: vte-driven grid + scrollback + snapshot serialization. Pure IO-free types.
//! Known gaps: no alt-screen grid swap, wide/combining chars treated as single-width, no scroll region, tab stops fixed at 8.

use std::collections::VecDeque;

use vte::{Params, Parser, Perform};

/// Scrollback limit honored by [`TerminalModel::scrollback_vt`].
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

/// `Default` means the terminal's default fg/bg (SGR `39`/`49`); `Indexed(0..=15)` map to ANSI 30-37/90-97, higher use `38;5;n`; `Rgb` uses `38;2;r;g;b`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    #[default]
    Default,
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// SGR (Select Graphic Rendition) state.
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

    /// The minimal SGR sequence that sets this state from a reset; always starts with `0;` so nothing is inherited.
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

/// One cell in the grid; `' '` with default SGR is the canonical blank cell.
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

/// Erase region selector matching xterm's CSI J / CSI K modes 0/1/2; CSI 3 J (also clear scrollback) is folded into `All`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EraseMode {
    /// From the cursor to the end of the line / screen (CSI 0 J / 0 K).
    ToEnd,
    /// From the start of the line / screen to the cursor (CSI 1 J / 1 K).
    ToStart,
    /// Entire line / screen (CSI 2 J / 2 K, and CSI 3 J).
    All,
}

/// Terminal state (grid + cursor + SGR + scrollback) consumed by [`VteProcessor`]; methods are named after VT semantics, not byte codes.
pub trait TerminalHandler {
    /// Print one printable character at the cursor; wide characters / combining marks are treated as single-width.
    fn print(&mut self, c: char);

    /// CR (0x0D) — move cursor to column 0 of the current row.
    fn carriage_return(&mut self);
    /// LF / VT / FF (0x0A..=0x0C) — cursor down one row, scrolling at the bottom.
    fn line_feed(&mut self);
    /// BS (0x08) — cursor one column left, no wrap to the previous row.
    fn backspace(&mut self);
    /// HT (0x09) — advance to the next 8-column tab stop, clamped to the last column.
    fn horizontal_tab(&mut self);
    /// BEL (0x07) — noop in this implementation.
    fn bell(&mut self);

    /// CUU (CSI A) — cursor up by `n`, saturating at row 0.
    fn cursor_up(&mut self, n: u16);
    /// CUD (CSI B / CSI e) — cursor down by `n`, clamped to last row.
    fn cursor_down(&mut self, n: u16);
    /// CUF (CSI C / CSI a) — cursor forward by `n`, clamped to the last column.
    fn cursor_forward(&mut self, n: u16);
    /// CUB (CSI D) — cursor back (left) by `n`, saturating at col 0.
    fn cursor_backward(&mut self, n: u16);

    /// CUP / HVP (CSI H / CSI f) — absolute cursor position, 0-indexed.
    fn cursor_to(&mut self, row: u16, col: u16);

    /// CHA / HPA (CSI G / CSI \`) — absolute column position, 0-indexed.
    fn cursor_column(&mut self, col: u16);

    /// VPA (CSI d) — absolute row position, 0-indexed.
    fn cursor_row(&mut self, row: u16);

    /// ED (CSI J) — erase in display, relative to the cursor.
    fn erase_screen(&mut self, mode: EraseMode);

    /// EL (CSI K) — erase in line, relative to the cursor.
    fn erase_line(&mut self, mode: EraseMode);

    /// SU (CSI S) — scroll the viewport up by `n`; the top rows move into scrollback (no scroll region).
    fn scroll_up(&mut self, n: u16);
    /// SD (CSI T) — scroll the viewport down by `n`; bottom rows dropped, top filled with blanks.
    fn scroll_down(&mut self, n: u16);

    /// SGR (CSI m); `params` is the already-flattened code sequence (extended colors arrive as consecutive elements).
    fn set_sgr(&mut self, params: &[u16]);

    /// DECTCEM (CSI ?25 h/l) — show or hide the cursor.
    fn set_cursor_visible(&mut self, visible: bool);

    /// DECSET 1049 — enter alternate screen. Only a flag for snapshot restore; grids are not swapped and `rev()` stays unchanged.
    fn enter_alt_screen(&mut self);

    /// DECRST 1049 — exit alternate screen. Same no-bump-rev invariant as enter.
    fn exit_alt_screen(&mut self);

    /// DECSET/DECRST mouse reporting (`CSI ? 9/1000/1002/1003/1006 h/l`). Default noop.
    fn set_mouse_mode(&mut self, _code: u16, _enabled: bool) {}

    /// DECSET/DECRST 2004 — bracketed paste. Default noop.
    fn set_bracketed_paste(&mut self, _enabled: bool) {}

    /// DECSET/DECRST 1004 — focus event reporting. Tracked purely as a capability signal (is the child a focus-aware TUI?);
    /// a mode flag, not visible content, so it MUST NOT bump the render rev.
    fn set_focus_event_tracking(&mut self, enabled: bool);

    /// OSC 10 / OSC 11 color query (`ESC ] slot ; ? ST`); the handler may push a reply into its pending-write buffer. Default noop.
    fn osc_color_query(&mut self, _slot: u8) {}

    /// DSR cursor position report (`CSI 6 n`); the child expects `ESC [ row;col R` (1-indexed), and codex's startup probe burns its full timeout if we stay silent. Default noop.
    fn device_status_report_cursor(&mut self) {}

    /// Kitty keyboard-enhancement query (`CSI ? u`); we support none, so the reply is `ESC [ ? 0 u`. Default noop.
    fn kitty_keyboard_query(&mut self) {}

    /// Primary device attributes (`CSI c` / `CSI 0 c`); answered with the minimum xterm-compatible `ESC [ ? 1 ; 0 c`. DA2/DA3 are NOT handled. Default noop.
    fn device_attributes_primary(&mut self) {}
}

/// VTE-to-handler adapter: implements `vte::Perform` and forwards each callback to the [`TerminalHandler`]; never mutates state directly.
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
        // DEC private (ESC[?...) sequences arrive with intermediates = b"?".
        if intermediates == b"?" {
            match action {
                'h' => {
                    for s in params.iter() {
                        if let Some(&p) = s.first() {
                            match p {
                                25 => self.handler.set_cursor_visible(true),
                                9 | 1000 | 1002 | 1003 | 1006 => {
                                    self.handler.set_mouse_mode(p, true)
                                }
                                1004 => self.handler.set_focus_event_tracking(true),
                                1049 => self.handler.enter_alt_screen(),
                                2004 => self.handler.set_bracketed_paste(true),
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
                                9 | 1000 | 1002 | 1003 | 1006 => {
                                    self.handler.set_mouse_mode(p, false)
                                }
                                1004 => self.handler.set_focus_event_tracking(false),
                                1049 => self.handler.exit_alt_screen(),
                                2004 => self.handler.set_bracketed_paste(false),
                                _ => { /* unknown DECRST: noop */ }
                            }
                        }
                    }
                }
                'u' => {
                    // Kitty keyboard-enhancement query (`CSI ? u`); codex probes this at startup and blocks on the response.
                    self.handler.kitty_keyboard_query();
                }
                _ => { /* unknown ?-CSI: noop */ }
            }
            return;
        }

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
                // Flatten (semicolon + colon subparams) into a single sequence; SGR walking lives in the handler.
                if params.is_empty() {
                    self.handler.set_sgr(&[]);
                } else {
                    let flat: Vec<u16> = params.iter().flat_map(|s| s.iter().copied()).collect();
                    self.handler.set_sgr(&flat);
                }
            }
            // DSR `CSI 6 n`; guard on empty intermediates so DEC-private variants like `CSI ? 6 n` are not mishandled.
            'n' if intermediates.is_empty() && Self::first_param_or(params, 0) == 6 => {
                self.handler.device_status_report_cursor();
            }
            // DA1: gate on empty intermediates so DA2 (`CSI > c`) / DA3 (`CSI = c`) are not answered; param defaults to 0 when omitted.
            'c' if intermediates.is_empty() && Self::first_param_or(params, 0) == 0 => {
                self.handler.device_attributes_primary();
            }
            // Unknown CSI: noop. NEVER panic — the child may emit anything.
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, _intermediates: &[u8], _ignore: bool, _byte: u8) {
        // ESC-only sequences (DECSC / DECRC / index / RI / charset selection): all noop.
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        // OSC 10/11 with `?` as the value is a theme query; the model pushes a reply into its pending-write buffer and the daemon flushes it after `feed()`.
        let Some(first) = params.first() else { return };
        let Some(second) = params.get(1) else { return };
        if *second != b"?" {
            return;
        }
        let slot = match *first {
            b"10" => 10u8,
            b"11" => 11u8,
            // OSC 12 (cursor color) is deliberately silent, like every other non-10/11 slot.
            _ => return,
        };
        self.handler.osc_color_query(slot);
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
}

/// High-level driver: owns the parser, grid, cursor, SGR state and scrollback; implements [`TerminalHandler`].
pub struct TerminalModel {
    parser: Parser,
    grid: Grid,
    cursor: Cursor,
    sgr: SgrState,
    scrollback: VecDeque<Vec<Cell>>,
    scrollback_max_lines: usize,
    rev: u32,
    cursor_visible: bool,
    /// DECSET 1004 state; the daemon reads it as a "focus-aware TUI" signal to gate the mid-session `ESC[I` theme nudge
    /// (a shell at the prompt would otherwise see a stray focus-in byte). Not visible content, so never bumps the render rev.
    focus_event_tracking: bool,
    /// DECSET 1049 flag; grids are not swapped, but reconnect snapshots must re-emit `CSI ?1049h`.
    alt_screen: bool,
    mouse_x10: bool,
    mouse_vt200: bool,
    mouse_drag: bool,
    mouse_any: bool,
    mouse_sgr: bool,
    bracketed_paste: bool,
    /// Default fg/bg advertised in reply to OSC 10/11 queries; `None` means stay silent and the child falls back to its built-in default.
    default_fg: Option<(u8, u8, u8)>,
    default_bg: Option<(u8, u8, u8)>,
    /// Bytes the daemon should push back onto the PTY master after the current `feed()` returns; drained via [`Self::take_pending_osc_replies`].
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
            alt_screen: false,
            mouse_x10: false,
            mouse_vt200: false,
            mouse_drag: false,
            mouse_any: false,
            mouse_sgr: false,
            bracketed_paste: false,
            default_fg: None,
            default_bg: None,
            pending_osc_replies: Vec::new(),
        }
    }

    /// Same as [`Self::new`] but pre-seeds the default fg/bg, so a child's startup OSC 10/11 probe gets an authoritative answer.
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

    /// Replace the default fg/bg; pre-existing `pending_osc_replies` are not rewritten.
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
    pub fn focus_event_tracking(&self) -> bool {
        self.focus_event_tracking
    }

    /// Drain any reply bytes the model produced since the last call; the daemon writes them to the PTY master after each `feed()`.
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
        // LF: cursor down; past the bottom, scroll up into scrollback. No scroll region (DECSTBM) tracking.
        if self.cursor.row + 1 >= self.grid.rows_count {
            self.scroll_up_inner(1);
        } else {
            self.cursor.row += 1;
        }
    }

    /// Feed raw PTY bytes through the parser. Each visible state change bumps `rev()` by 1.
    pub fn feed(&mut self, bytes: &[u8]) {
        // Take the parser out so `&mut self` can be handed to the processor.
        let mut parser = std::mem::replace(&mut self.parser, Parser::new());
        {
            let mut processor = VteProcessor::new(self);
            for &b in bytes {
                parser.advance(&mut processor, b);
            }
        }
        self.parser = parser;
    }

    /// Resize the internal grid: cols clip/pad, reduced rows keep the active tail and evict older top lines into scrollback. Always bumps `rev`.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        let new_cols = cols.max(1);
        let new_rows = rows.max(1);
        if new_cols == self.grid.cols && new_rows == self.grid.rows_count {
            // Identity resize: still bump so the daemon can decide to re-broadcast a snapshot.
            self.bump();
            return;
        }

        // Rows must remain exactly grid.cols wide: retaining hidden suffixes would let stale text reappear after a widen.
        if new_cols != self.grid.cols {
            let new_cols_usize = new_cols as usize;
            for row in self.grid.rows.iter_mut() {
                row.resize(new_cols_usize, Cell::default());
            }
            self.grid.cols = new_cols;
        }

        let cur_rows = self.grid.rows.len();
        let target_rows = new_rows as usize;
        match target_rows.cmp(&cur_rows) {
            std::cmp::Ordering::Greater => {
                let blank_row = vec![Cell::default(); new_cols as usize];
                for _ in cur_rows..target_rows {
                    self.grid.rows.push(blank_row.clone());
                }
            }
            std::cmp::Ordering::Less => {
                // Trim genuinely unused rows below the cursor first; otherwise anchor to the active tail and evict from the top into scrollback.
                let to_drop = cur_rows - target_rows;
                for _ in 0..to_drop {
                    let last_idx = self.grid.rows.len().saturating_sub(1);
                    let bottom_is_unused = last_idx > self.cursor.row as usize
                        && self.grid.rows[last_idx].iter().all(Cell::is_blank);
                    if bottom_is_unused {
                        self.grid.rows.pop();
                    } else if !self.grid.rows.is_empty() {
                        let dropped = self.grid.rows.remove(0);
                        self.scrollback.push_back(dropped);
                        while self.scrollback.len() > self.scrollback_max_lines {
                            self.scrollback.pop_front();
                        }
                        self.cursor.row = self.cursor.row.saturating_sub(1);
                    }
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

    fn emit_host_modes(&self, out: &mut Vec<u8>) {
        let mut push = |n: u16| {
            out.extend_from_slice(format!("\x1b[?{n}h").as_bytes());
        };
        if self.alt_screen {
            push(1049);
        }
        if self.mouse_x10 {
            push(9);
        }
        if self.mouse_vt200 {
            push(1000);
        }
        if self.mouse_drag {
            push(1002);
        }
        if self.mouse_any {
            push(1003);
        }
        if self.mouse_sgr {
            push(1006);
        }
        if self.focus_event_tracking {
            push(1004);
        }
        if self.bracketed_paste {
            push(2004);
        }
    }

    pub fn snapshot_vt(&self, target_cols: u16, target_rows: u16) -> Vec<u8> {
        let mut out = Vec::with_capacity(target_cols as usize * target_rows as usize * 2);
        // Re-enter host modes *before* painting: a remounted xterm.js starts with mouse reporting off, so clicks would never reach the child.
        self.emit_host_modes(&mut out);
        out.extend_from_slice(b"\x1b[?25l\x1b[2J\x1b[H");

        let target_cols = target_cols.max(1);
        let target_rows = target_rows.max(1);

        for row_idx in 0..target_rows {
            let pos = format!("\x1b[{};1H", row_idx + 1);
            out.extend_from_slice(pos.as_bytes());
            out.extend_from_slice(b"\x1b[0m");

            let mut last_sgr = SgrState::default();

            // Skip the trailing blank run; `ESC[K` after the loop wipes the remainder.
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
            // Clear to end of line — also covers cols past the internal grid when `target_cols > grid.cols`.
            out.extend_from_slice(b"\x1b[K");
        }

        out.extend_from_slice(b"\x1b[0m");
        let cur = self.cursor;
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

    /// Serialize the scrollback as ANSI bytes, to be written BEFORE `snapshot_vt` so it lands in the client's own
    /// scrollback ring; lines beyond `limit` are dropped from the front (oldest first).
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

// All grid/cursor/SGR/scrollback mutation lives here; every method bumps `rev` once per visible state change.
impl TerminalHandler for TerminalModel {
    fn print(&mut self, c: char) {
        let cell = Cell {
            ch: c,
            sgr: self.sgr,
        };
        self.clamp_cursor();
        self.grid.set_cell(self.cursor.row, self.cursor.col, cell);
        if self.cursor.col + 1 < self.grid.cols {
            self.cursor.col += 1;
        } else {
            // End of line: stay at the last column; xterm's pending-wrap flag is intentionally simplified.
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
        // Extended color sequences (38;5;n / 38;2;r;g;b) arrive pre-flattened.
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
        // Flag only: we still paint into the main grid.
        self.alt_screen = true;
    }

    fn exit_alt_screen(&mut self) {
        self.alt_screen = false;
    }

    fn set_mouse_mode(&mut self, code: u16, enabled: bool) {
        match code {
            9 => self.mouse_x10 = enabled,
            1000 => self.mouse_vt200 = enabled,
            1002 => self.mouse_drag = enabled,
            1003 => self.mouse_any = enabled,
            1006 => self.mouse_sgr = enabled,
            _ => {}
        }
    }

    fn set_bracketed_paste(&mut self, enabled: bool) {
        self.bracketed_paste = enabled;
    }

    fn set_focus_event_tracking(&mut self, enabled: bool) {
        // Pure mode flag — do NOT bump rev (no visible state change).
        self.focus_event_tracking = enabled;
    }

    fn device_status_report_cursor(&mut self) {
        // `ESC [ row;col R`, both 1-indexed on the wire; codex blocks on this during startup.
        let row1 = self.cursor.row.saturating_add(1);
        let col1 = self.cursor.col.saturating_add(1);
        let reply = format!("\x1b[{};{}R", row1, col1);
        self.pending_osc_replies.extend_from_slice(reply.as_bytes());
    }

    fn kitty_keyboard_query(&mut self) {
        // flags=0: no kitty keyboard-protocol enhancements, so the child falls back to legacy keycoding.
        self.pending_osc_replies.extend_from_slice(b"\x1b[?0u");
    }

    fn device_attributes_primary(&mut self) {
        // DA1 reply `ESC [ ? 1 ; 0 c` ("VT101, no options"), the minimum xterm-compatible response.
        self.pending_osc_replies.extend_from_slice(b"\x1b[?1;0c");
    }

    fn osc_color_query(&mut self, slot: u8) {
        // xterm replies with the 16-bit form `rgb:RRRR/GGGG/BBBB`: each 8-bit channel becomes `c * 257`, terminated with ST.
        let rgb = match slot {
            10 => self.default_fg,
            11 => self.default_bg,
            _ => return,
        };
        let Some((r, g, b)) = rgb else {
            // No color configured → stay silent; the child falls back to its built-in default.
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
