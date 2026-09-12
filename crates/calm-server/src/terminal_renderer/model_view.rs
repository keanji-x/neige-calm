//! A read-only RMUX projection of the actual renderer input stream. It never
//! reconstructs from the legacy renderer's lossy ANSI serialization.
use calm_session::terminal_session::RenderObserver;
use calm_terminal_view::{Frame, TerminalView};
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

pub type SharedModelView = Arc<Mutex<ModelView>>;
pub struct ModelView {
    view: anyhow::Result<TerminalView>,
    revision: u64,
    /// Published under the same lock as `revision`, so a subscriber that
    /// observes a value on this channel can capture at least that revision.
    /// Invalidation also wakes subscribers; they discover the error on capture.
    published: watch::Sender<u64>,
}
impl ModelView {
    pub fn new(cols: u16, rows: u16, fg: (u8, u8, u8), bg: (u8, u8, u8)) -> SharedModelView {
        Arc::new(Mutex::new(Self {
            view: TerminalView::new(cols, rows, [fg.0, fg.1, fg.2], [bg.0, bg.1, bg.2]),
            revision: 0,
            published: watch::channel(0u64).0,
        }))
    }
    /// Revision notifications for change waiting; no polling is required.
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.published.subscribe()
    }
    /// Live revision subscribers: a change wait counts from the moment it
    /// subscribes until it returns. Test observability of a wait in progress.
    pub fn change_waiters(&self) -> usize {
        self.published.receiver_count()
    }
    pub fn capture(&self, offset: usize) -> anyhow::Result<(Frame, u64)> {
        let view = self
            .view
            .as_ref()
            .map_err(|error| anyhow::anyhow!("terminal projection unavailable: {error}"))?;
        Ok((view.frame(offset)?, self.revision))
    }
    pub fn invalidate(&mut self, reason: &str) {
        self.view = Err(anyhow::anyhow!("{reason}"));
        self.published.send_modify(|_| {});
    }
    fn advance(&mut self) {
        match self.revision.checked_add(1) {
            Some(next) => {
                self.revision = next;
                self.published.send_replace(next);
            }
            None => self.invalidate("observation sequence exhausted"),
        }
    }
}
pub struct Observer(pub SharedModelView);
impl RenderObserver for Observer {
    fn unavailable(&mut self, reason: &str) {
        if let Ok(mut state) = self.0.lock() {
            state.invalidate(reason);
        }
    }
    fn output(&mut self, bytes: &[u8]) {
        if let Ok(mut state) = self.0.lock() {
            if let Ok(view) = &mut state.view {
                view.feed(bytes);
            }
            state.advance();
        }
    }
    fn resize(&mut self, cols: u16, rows: u16) {
        if let Ok(mut state) = self.0.lock() {
            if let Ok(view) = &mut state.view
                && let Err(error) = view.resize(cols, rows)
            {
                state.view = Err(error);
            }
            state.advance();
        }
    }
    fn colors(&mut self, fg: Option<(u8, u8, u8)>, bg: Option<(u8, u8, u8)>) {
        if let Ok(mut state) = self.0.lock() {
            if let Ok(view) = &mut state.view {
                view.colors(fg.map(|c| [c.0, c.1, c.2]), bg.map(|c| [c.0, c.1, c.2]));
            }
            state.advance();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use calm_session::terminal_model::ScrollbackLimit;
    use calm_session::terminal_session::RenderPlane;
    #[test]
    fn observation_survives_legacy_snapshot_resize_and_cjk_key_modes() {
        let mut plane = RenderPlane::new(20, 3, 8192, 2000);
        let observed = ModelView::new(20, 3, (220, 220, 220), (15, 20, 24));
        plane.install_observer(Box::new(Observer(observed.clone())));
        for index in 0..10 {
            plane.on_pty_chunk(format!("line{index}\r\n").into_bytes());
        }
        plane.on_pty_chunk("中文\x1b[?1h".as_bytes().to_vec());
        let (before, _) = observed.lock().unwrap().capture(0).unwrap();
        assert_eq!(before.cursor.column, 4);
        assert!(before.history_rows > 0);
        let history = before.history_rows;
        assert_eq!(
            calm_terminal_view::key_bytes("Up", before.modes).unwrap(),
            b"\x1bOA"
        );
        // Actual legacy recovery generation must not replace the model projection.
        plane.build_snapshot(20, 3, ScrollbackLimit::All);
        plane.on_resize(22, 3);
        let (after, _) = observed.lock().unwrap().capture(0).unwrap();
        assert_eq!(after.cursor.column, 4);
        assert_eq!(after.history_rows, history);
        assert_eq!(
            calm_terminal_view::key_bytes("Up", after.modes).unwrap(),
            b"\x1bOA"
        );
        assert!(after.text.iter().any(|line| line == "中文"));
        assert_eq!(after.cols, 22);
    }
    #[test]
    fn pure_mode_chunks_advance_observation_and_source_gaps_fail_closed() {
        let mut plane = RenderPlane::new(80, 24, 8192, 2000);
        let observed = ModelView::new(80, 24, (220, 220, 220), (15, 20, 24));
        plane.install_observer(Box::new(Observer(observed.clone())));
        let (_, prior) = observed.lock().unwrap().capture(0).unwrap();
        plane.on_pty_chunk(b"\x1b[?1h".to_vec());
        let (frame, revision) = observed.lock().unwrap().capture(0).unwrap();
        assert!(revision > prior);
        assert_eq!(
            calm_terminal_view::key_bytes("Up", frame.modes).unwrap(),
            b"\x1bOA"
        );
        plane.invalidate_observation("simulated source gap");
        assert!(observed.lock().unwrap().capture(0).is_err());
        plane.on_pty_chunk(b"more bytes".to_vec());
        assert!(
            observed.lock().unwrap().capture(0).is_err(),
            "later bytes cannot repair missing state"
        );
    }
}
