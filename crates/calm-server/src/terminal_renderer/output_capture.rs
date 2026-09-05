use std::sync::{Arc, Mutex};

/// Durable task history is intentionally much smaller than the supervisor's
/// 1 MiB replay ring. Keep the tail: command failures and summaries normally
/// land at the end, and the explicit truncation bit prevents false completeness.
pub(crate) const TERMINAL_OUTPUT_MAX_BYTES: usize = 8 * 1024;

pub(crate) type SharedTerminalOutputCapture = Arc<Mutex<TerminalOutputCapture>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TerminalOutputSnapshot {
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug)]
pub(crate) struct TerminalOutputCapture {
    tail: Vec<u8>,
    truncated: bool,
}

impl TerminalOutputCapture {
    pub fn shared(initial_cursor_head: u64, replay: &[u8]) -> SharedTerminalOutputCapture {
        let mut capture = Self {
            tail: Vec::with_capacity(TERMINAL_OUTPUT_MAX_BYTES.min(replay.len())),
            truncated: initial_cursor_head > 0,
        };
        capture.push(replay);
        Arc::new(Mutex::new(capture))
    }

    pub fn push(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        if bytes.len() > TERMINAL_OUTPUT_MAX_BYTES {
            self.tail.clear();
            self.tail
                .extend_from_slice(&bytes[bytes.len() - TERMINAL_OUTPUT_MAX_BYTES..]);
            self.truncated = true;
            return;
        }
        let overflow = self
            .tail
            .len()
            .saturating_add(bytes.len())
            .saturating_sub(TERMINAL_OUTPUT_MAX_BYTES);
        if overflow > 0 {
            self.tail.drain(..overflow);
            self.truncated = true;
        }
        self.tail.extend_from_slice(bytes);
    }

    pub fn mark_gap(&mut self) {
        self.truncated = true;
    }

    pub fn snapshot(&self) -> TerminalOutputSnapshot {
        TerminalOutputSnapshot {
            text: String::from_utf8_lossy(&self.tail).into_owned(),
            truncated: self.truncated,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capture_keeps_bounded_tail_and_marks_truncation() {
        let capture = TerminalOutputCapture::shared(0, b"prefix");
        let oversized = vec![b'x'; TERMINAL_OUTPUT_MAX_BYTES + 7];
        capture.lock().unwrap().push(&oversized);
        let snapshot = capture.lock().unwrap().snapshot();

        assert_eq!(snapshot.text.len(), TERMINAL_OUTPUT_MAX_BYTES);
        assert!(snapshot.text.bytes().all(|byte| byte == b'x'));
        assert!(snapshot.truncated);
    }

    #[test]
    fn exact_cap_without_prior_bytes_is_complete() {
        let exact = vec![b'x'; TERMINAL_OUTPUT_MAX_BYTES];
        let capture = TerminalOutputCapture::shared(0, &exact);
        let snapshot = capture.lock().unwrap().snapshot();

        assert_eq!(snapshot.text.len(), TERMINAL_OUTPUT_MAX_BYTES);
        assert!(!snapshot.truncated, "no byte was dropped at the exact cap");
    }

    #[test]
    fn replay_head_and_later_gap_are_both_truncation_evidence() {
        let capture = TerminalOutputCapture::shared(9, b"tail");
        assert!(capture.lock().unwrap().snapshot().truncated);

        let capture = TerminalOutputCapture::shared(0, b"whole");
        assert!(!capture.lock().unwrap().snapshot().truncated);
        capture.lock().unwrap().mark_gap();
        assert!(capture.lock().unwrap().snapshot().truncated);
    }
}
