//! Legacy spec-push runtime module.
//!
//! PR-del D6 retires the old in-process queue, registry, notification
//! consumer, recovery signals, and public phase/action API. The remaining
//! push-lock proof type moved to `harness::PushLockGuard` in PR-del D7.
