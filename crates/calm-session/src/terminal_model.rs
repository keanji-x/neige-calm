//! Server-side terminal model: vte-driven grid + scrollback + snapshot
//! serialization.
//!
//! Pure IO-free types. No tokio, no `Arc`/`Mutex`. The render plane in
//! [`crate::terminal_session::RenderPlane`] owns one [`TerminalModel`]; the
//! daemon shell feeds it raw PTY bytes via [`TerminalModel::feed`].
//!
//! ## Pipeline
//!
//! 1. PTY chunk arrives → `RenderPlane::on_pty_chunk(bytes)`.
//! 2. `feed(bytes)` drives `vte::Parser` byte-by-byte into [`Performer`],
//!    which mutates a self-built cell grid + cursor + SGR state and
//!    bumps `rev` once per visible state change.
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
//! - **OSC** (title, hyperlink, color queries) — ignored.
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

/// Performer: implements `vte::Perform`, mutates the grid + cursor + SGR.
///
/// Public because [`TerminalModel`] exposes it for `feed`/`snapshot`; not
/// meant for external use.
pub struct Performer {
    grid: Grid,
    cursor: Cursor,
    sgr: SgrState,
    scrollback: VecDeque<Vec<Cell>>,
    scrollback_max_lines: usize,
    rev: u32,
    cursor_visible: bool,
}

impl Performer {
    fn new(cols: u16, rows: u16, scrollback_max_lines: usize) -> Self {
        Self {
            grid: Grid::new(cols, rows),
            cursor: Cursor::default(),
            sgr: SgrState::default(),
            scrollback: VecDeque::new(),
            scrollback_max_lines,
            rev: 0,
            cursor_visible: true,
        }
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

    fn scroll_up(&mut self, n: u16) {
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

    fn scroll_down(&mut self, n: u16) {
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
            self.scroll_up(1);
        } else {
            self.cursor.row += 1;
        }
    }

    // ---- CSI helpers ----

    fn first_param_or(&self, params: &Params, default: u16) -> u16 {
        params
            .iter()
            .next()
            .and_then(|s| s.first().copied())
            .filter(|v| *v != 0)
            .unwrap_or(default)
    }

    fn handle_cup(&mut self, params: &Params) {
        // CUP / HVP: 1-indexed (row, col).
        let mut it = params.iter();
        let row1 = it.next().and_then(|s| s.first().copied()).unwrap_or(1);
        let col1 = it.next().and_then(|s| s.first().copied()).unwrap_or(1);
        let row = row1.saturating_sub(1);
        let col = col1.saturating_sub(1);
        self.cursor.row = row.min(self.grid.rows_count.saturating_sub(1));
        self.cursor.col = col.min(self.grid.cols.saturating_sub(1));
    }

    fn handle_cuu(&mut self, params: &Params) {
        let n = self.first_param_or(params, 1);
        self.cursor.row = self.cursor.row.saturating_sub(n);
    }

    fn handle_cud(&mut self, params: &Params) {
        let n = self.first_param_or(params, 1);
        let new_row = self.cursor.row.saturating_add(n);
        self.cursor.row = new_row.min(self.grid.rows_count.saturating_sub(1));
    }

    fn handle_cuf(&mut self, params: &Params) {
        let n = self.first_param_or(params, 1);
        let new_col = self.cursor.col.saturating_add(n);
        self.cursor.col = new_col.min(self.grid.cols.saturating_sub(1));
    }

    fn handle_cub(&mut self, params: &Params) {
        let n = self.first_param_or(params, 1);
        self.cursor.col = self.cursor.col.saturating_sub(n);
    }

    fn handle_ed(&mut self, params: &Params) {
        let mode = params
            .iter()
            .next()
            .and_then(|s| s.first().copied())
            .unwrap_or(0);
        match mode {
            // 0: cursor to end of screen.
            0 => {
                self.grid.clear_row_from(self.cursor.row, self.cursor.col);
                for r in (self.cursor.row + 1)..self.grid.rows_count {
                    self.grid.clear_row(r);
                }
            }
            // 1: start to cursor.
            1 => {
                for r in 0..self.cursor.row {
                    self.grid.clear_row(r);
                }
                self.grid.clear_row_to(self.cursor.row, self.cursor.col);
            }
            // 2: entire screen. (3 = also scrollback — we treat as 2.)
            _ => self.grid.clear_all(),
        }
    }

    fn handle_el(&mut self, params: &Params) {
        let mode = params
            .iter()
            .next()
            .and_then(|s| s.first().copied())
            .unwrap_or(0);
        match mode {
            0 => self.grid.clear_row_from(self.cursor.row, self.cursor.col),
            1 => self.grid.clear_row_to(self.cursor.row, self.cursor.col),
            _ => self.grid.clear_row(self.cursor.row),
        }
    }

    fn handle_sgr(&mut self, params: &Params) {
        if params.is_empty() {
            self.sgr.reset();
            return;
        }
        // Iterate by param-position, but `iter()` yields `&[u16]` slices
        // (subparams). For our purposes the first u16 in each slice IS
        // the param. Extended color sequences (38;5;n / 38;2;r;g;b) are
        // emitted as a *single* param with subparams when the client uses
        // colon separators; we handle both colon and semicolon form by
        // flattening into a single Vec<u16>.
        let flat: Vec<u16> = params.iter().flat_map(|s| s.iter().copied()).collect();
        let mut i = 0;
        while i < flat.len() {
            let p = flat[i];
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
                    if let Some(&kind) = flat.get(i + 1) {
                        if kind == 5
                            && let Some(&n) = flat.get(i + 2)
                        {
                            self.sgr.fg = Color::Indexed((n & 0xFF) as u8);
                            i += 2;
                        } else if kind == 2
                            && let (Some(&r), Some(&g), Some(&b)) =
                                (flat.get(i + 2), flat.get(i + 3), flat.get(i + 4))
                        {
                            self.sgr.fg = Color::Rgb(r as u8, g as u8, b as u8);
                            i += 4;
                        }
                    }
                }
                39 => self.sgr.fg = Color::Default,
                40..=47 => self.sgr.bg = Color::Indexed((p - 40) as u8),
                48 => {
                    if let Some(&kind) = flat.get(i + 1) {
                        if kind == 5
                            && let Some(&n) = flat.get(i + 2)
                        {
                            self.sgr.bg = Color::Indexed((n & 0xFF) as u8);
                            i += 2;
                        } else if kind == 2
                            && let (Some(&r), Some(&g), Some(&b)) =
                                (flat.get(i + 2), flat.get(i + 3), flat.get(i + 4))
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
    }
}

impl Perform for Performer {
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

    fn execute(&mut self, byte: u8) {
        match byte {
            0x07 => { /* BEL: noop */ }
            0x08 if self.cursor.col > 0 => {
                // BS
                self.cursor.col -= 1;
                self.bump();
            }
            0x09 => {
                // HT: jump to next 8-col boundary.
                let next = (self.cursor.col / 8 + 1) * 8;
                let max = self.grid.cols.saturating_sub(1);
                self.cursor.col = next.min(max);
                self.bump();
            }
            0x0a..=0x0c => {
                // LF / VT / FF — all treated as newline (xterm default).
                self.newline();
                self.bump();
            }
            0x0d => {
                // CR
                self.cursor.col = 0;
                self.bump();
            }
            _ => { /* other C0 controls noop */ }
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        // DEC private (ESC[?...) sequences arrive with intermediates = b"?".
        // We only care about cursor visibility — alternate-screen
        // (1049) is explicitly a noop per the EXPERIMENTAL note.
        if intermediates == b"?" {
            match action {
                'h' => {
                    // DECSET — process every param that we care about.
                    // 1049 alternate screen: noop (see module doc).
                    // Other DECSET codes: noop.
                    for s in params.iter() {
                        if let Some(&p) = s.first()
                            && p == 25
                        {
                            self.cursor_visible = true;
                            self.bump();
                        }
                    }
                }
                'l' => {
                    for s in params.iter() {
                        if let Some(&p) = s.first()
                            && p == 25
                        {
                            self.cursor_visible = false;
                            self.bump();
                        }
                    }
                }
                _ => { /* unknown ?-CSI: noop */ }
            }
            return;
        }
        // Vanilla CSI (no intermediates of interest).
        match action {
            'A' => {
                self.handle_cuu(params);
                self.bump();
            }
            'B' | 'e' => {
                self.handle_cud(params);
                self.bump();
            }
            'C' | 'a' => {
                self.handle_cuf(params);
                self.bump();
            }
            'D' => {
                self.handle_cub(params);
                self.bump();
            }
            'H' | 'f' => {
                self.handle_cup(params);
                self.bump();
            }
            'G' | '`' => {
                // CHA / HPA: 1-indexed column.
                let col1 = self.first_param_or(params, 1);
                self.cursor.col = col1.saturating_sub(1).min(self.grid.cols.saturating_sub(1));
                self.bump();
            }
            'd' => {
                // VPA: 1-indexed row.
                let row1 = self.first_param_or(params, 1);
                self.cursor.row = row1
                    .saturating_sub(1)
                    .min(self.grid.rows_count.saturating_sub(1));
                self.bump();
            }
            'J' => {
                self.handle_ed(params);
                self.bump();
            }
            'K' => {
                self.handle_el(params);
                self.bump();
            }
            'S' => {
                let n = self.first_param_or(params, 1);
                self.scroll_up(n);
                self.bump();
            }
            'T' => {
                let n = self.first_param_or(params, 1);
                self.scroll_down(n);
                self.bump();
            }
            'm' => {
                self.handle_sgr(params);
                self.bump();
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

    fn osc_dispatch(&mut self, _params: &[&[u8]], _bell_terminated: bool) {
        // OSC: window title, hyperlinks, palette queries — all ignored.
    }

    fn hook(&mut self, _params: &Params, _intermediates: &[u8], _ignore: bool, _action: char) {}
    fn put(&mut self, _byte: u8) {}
    fn unhook(&mut self) {}
}

/// High-level driver: owns the [`vte::Parser`] and [`Performer`]; exposes
/// `feed` / `resize` / `snapshot_vt` / `scrollback_vt`.
pub struct TerminalModel {
    parser: Parser,
    performer: Performer,
}

impl TerminalModel {
    pub fn new(cols: u16, rows: u16, scrollback_max_lines: usize) -> Self {
        Self {
            parser: Parser::new(),
            performer: Performer::new(cols, rows, scrollback_max_lines),
        }
    }

    /// Feed raw PTY bytes through the parser. Each visible state change
    /// bumps `rev()` by 1. Empty input bumps nothing.
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.parser.advance(&mut self.performer, b);
        }
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
        if new_cols == self.performer.grid.cols && new_rows == self.performer.grid.rows_count {
            // Identity resize: still bump so the daemon can decide to
            // re-broadcast a snapshot. Cheap, infrequent.
            self.performer.bump();
            return;
        }

        // 1. Adjust per-row width.
        if new_cols != self.performer.grid.cols {
            let new_cols_usize = new_cols as usize;
            for row in self.performer.grid.rows.iter_mut() {
                row.resize(new_cols_usize, Cell::default());
            }
            self.performer.grid.cols = new_cols;
        }

        // 2. Adjust row count.
        let cur_rows = self.performer.grid.rows.len();
        let target_rows = new_rows as usize;
        match target_rows.cmp(&cur_rows) {
            std::cmp::Ordering::Greater => {
                // Pad below.
                let blank_row = vec![Cell::default(); new_cols as usize];
                for _ in cur_rows..target_rows {
                    self.performer.grid.rows.push(blank_row.clone());
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
                if (self.performer.cursor.row as usize) < target_rows {
                    for _ in 0..to_drop {
                        self.performer.grid.rows.pop();
                    }
                } else {
                    for _ in 0..to_drop {
                        if !self.performer.grid.rows.is_empty() {
                            let dropped = self.performer.grid.rows.remove(0);
                            self.performer.scrollback.push_back(dropped);
                            while self.performer.scrollback.len()
                                > self.performer.scrollback_max_lines
                            {
                                self.performer.scrollback.pop_front();
                            }
                        }
                    }
                    self.performer.cursor.row =
                        self.performer.cursor.row.saturating_sub(to_drop as u16);
                }
            }
            std::cmp::Ordering::Equal => {}
        }
        self.performer.grid.rows_count = new_rows;
        self.performer.clamp_cursor();
        self.performer.bump();
    }

    pub fn rev(&self) -> u32 {
        self.performer.rev
    }

    pub fn size(&self) -> (u16, u16) {
        (self.performer.grid.cols, self.performer.grid.rows_count)
    }

    pub fn cursor(&self) -> Cursor {
        self.performer.cursor
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
                    let cell = self.performer.grid.cell(row_idx, col_idx);
                    if !cell.is_blank() {
                        found = Some(col_idx);
                    }
                }
                found
            };

            if let Some(end) = last_non_blank {
                for col_idx in 0..=end {
                    let cell = self.performer.grid.cell(row_idx, col_idx);
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
        let cur = self.performer.cursor;
        // Clamp cursor into the target geometry.
        let row = (cur.row.min(target_rows.saturating_sub(1))) + 1;
        let col = (cur.col.min(target_cols.saturating_sub(1))) + 1;
        let pos = format!("\x1b[{};{}H", row, col);
        out.extend_from_slice(pos.as_bytes());
        if self.performer.cursor_visible {
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
            ScrollbackLimit::All => self.performer.scrollback.len(),
            ScrollbackLimit::Lines(n) => (n as usize).min(self.performer.scrollback.len()),
        };
        if max == 0 {
            return Vec::new();
        }
        let start = self.performer.scrollback.len() - max;
        let mut out = Vec::with_capacity(max * self.performer.grid.cols as usize * 2);
        for line in self.performer.scrollback.iter().skip(start) {
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
        assert!(m.performer.sgr.bold);
        m.feed(b"\x1b[0m");
        assert!(!m.performer.sgr.bold);
        assert_eq!(m.performer.sgr.fg, Color::Default);
    }
}
