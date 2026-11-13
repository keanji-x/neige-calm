//! Presentation defaults shared by launch, viewer and deletion attachment.
use super::*;

pub(crate) const INITIAL_COLS: u16 = 80;
pub(crate) const INITIAL_ROWS: u16 = 24;
pub(crate) const REPLAY_BYTES: usize = 1 << 20;

impl RendererConfig {
    /// Observation only: this configuration never grants permission to launch.
    pub(crate) fn observation(
        term: &crate::model::Terminal,
        socket: PathBuf,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            terminal_id: term.id.clone(),
            cols: INITIAL_COLS,
            rows: INITIAL_ROWS,
            buffer_bytes: REPLAY_BYTES,
            terminal_fg: parse_rgb(&term.theme_fg).map_err(anyhow::Error::msg)?,
            terminal_bg: parse_rgb(&term.theme_bg).map_err(anyhow::Error::msg)?,
            program: term.program.clone(),
            args: vec![],
            envs: vec![],
            cwd: term.cwd.clone(),
            supervisor_sock: socket,
        })
    }
}

pub(crate) fn parse_rgb(s: &str) -> Result<(u8, u8, u8), String> {
    let parts: Vec<&str> = s.split(',').collect();
    if parts.len() != 3 {
        return Err(format!(
            "expected `r,g,b` (three comma-separated u8 channels), got {s:?}"
        ));
    }
    let parse = |i: usize| -> Result<u8, String> {
        parts[i]
            .trim()
            .parse::<u8>()
            .map_err(|e| format!("channel {i} ({:?}): {e}", parts[i]))
    };
    Ok((parse(0)?, parse(1)?, parse(2)?))
}
