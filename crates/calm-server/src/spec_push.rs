//! Legacy spec-push runtime module.
//!
//! PR-del D6 retires the old in-process queue, registry, notification
//! consumer, recovery signals, and public phase/action API. The remaining
//! push-lock proof type intentionally still lives in `dispatcher.rs`; D7 moves
//! it into the harness module.
