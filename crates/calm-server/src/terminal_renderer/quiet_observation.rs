//! Synchronous qualified observations; never retain a lock across SQL.
use super::*;

impl TerminalRendererRegistry {
    /// Registry identity and source liveness are protected through the final stamp
    /// read. No guard escapes this synchronous observation point.
    pub(crate) fn observe_quiet(
        &self,
        expected: &Arc<RendererEntry>,
        stamp: i64,
        observed_at: i64,
        threshold: i64,
    ) -> bool {
        let Ok(entries) = self.entries.lock() else {
            return false;
        };
        let Some(entry) = entries.get(&expected.terminal_id) else {
            return false;
        };
        if !Arc::ptr_eq(entry, expected) {
            return false;
        }
        {
            let Ok(reader) = entry.attach_task.lock() else {
                return false;
            };
            if reader.as_ref().is_some_and(JoinHandle::is_finished)
                || (entry.control_via_engine && reader.is_none())
            {
                return false;
            }
        }
        let Ok(_plane) = entry.handle.render_plane.lock() else {
            return false;
        };
        let Ok(model) = entry.handle.model_view.lock() else {
            return false;
        };
        let Ok(exit) = entry.exit.lock() else {
            return false;
        };
        if exit.is_some() || model.capture(0).is_err() {
            return false;
        }
        let actual = entry.last_output_ms.load(Ordering::SeqCst);
        actual > 0
            && actual == stamp
            && threshold > 0
            && observed_at
                .checked_sub(actual)
                .is_some_and(|age| age >= threshold)
    }
}

#[cfg(all(test, feature = "fixtures"))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn ended_reader_refuses_observation_even_while_its_last_screen_remains_readable() {
        let renderer = TerminalRendererRegistry::new();
        let entry = renderer.insert_test_entry(RendererConfig {
            terminal_id: "quiet-reader".into(),
            cols: 80,
            rows: 24,
            buffer_bytes: 8192,
            terminal_fg: (220, 220, 220),
            terminal_bg: (15, 20, 24),
            program: "/bin/sh".into(),
            args: vec![],
            envs: vec![],
            cwd: "/tmp".into(),
            supervisor_sock: Default::default(),
        });
        entry.last_output_ms.store(1000, Ordering::SeqCst);
        assert!(renderer.observe_quiet(&entry, 1000, 6000, 5000));
        let reader = tokio::spawn(async {});
        while !reader.is_finished() {
            tokio::task::yield_now().await;
        }
        *entry.attach_task.lock().unwrap() = Some(reader);
        assert!(entry.observable());
        assert!(!renderer.observe_quiet(&entry, 1000, 6000, 5000));
    }
}
