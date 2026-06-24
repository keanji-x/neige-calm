/// Print a skip marker and return early when the codex/auth env is absent.
#[macro_export]
macro_rules! skip {
    ($($arg:tt)*) => {{
        eprintln!("[codex-e2e] SKIP: {}", format!($($arg)*));
        return;
    }};
}
