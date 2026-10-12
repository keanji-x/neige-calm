//! Process exit codes, owned in one place.
//!
//! Only outcomes that need a human exit non-zero, so the systemd unit's
//! `OnFailure=` fires for them alone. A start that finds the archive busy or
//! no open window is a normal start and exits 0.

/// The command did what it was asked.
pub const OK: i32 = 0;
/// Another process holds the archive lock. Not an alert.
pub const BUSY: i32 = 0;
/// No capture window is open now. Not an alert.
pub const NO_WINDOW: i32 = 0;
/// A window closed with its run ended `partial`. Needs a look, not a login.
pub const PARTIAL: i32 = 3;
/// Anything else that stops the command: bad configuration, I/O, a refused
/// environment (sysexits `EX_SOFTWARE`).
pub const FATAL: i32 = 70;
/// No usable token: missing, malformed, expired without a working refresh, or
/// the SDK asked for the browser flow. Needs `auth import` or `auth login`
/// (sysexits `EX_TEMPFAIL`).
pub const AUTH_UNAVAILABLE: i32 = 75;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alerting_codes_are_distinct_and_non_zero() {
        let alerting = [PARTIAL, FATAL, AUTH_UNAVAILABLE];
        for (i, a) in alerting.iter().enumerate() {
            assert_ne!(*a, 0);
            // 1 is a Rust `main` error, 2 a clap usage error, 101 a panic.
            assert!(![1, 2, 101].contains(a), "{a}");
            assert!(!alerting[i + 1..].contains(a), "{a} repeats");
        }
        assert_eq!([OK, BUSY, NO_WINDOW], [0, 0, 0]);
    }
}
