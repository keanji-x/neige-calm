//! The SDK runs `dotenv()` when an HTTP client is configured, which loads the
//! first `.env` in the working directory or a parent into the test process
//! (adding `NO_PROXY`, `LONGBRIDGE_ENV`, …). A test that builds an SDK
//! client runs only where no `.env` is in reach. The search is the
//! binary's own guard, [`research_archive::env_guard::check`].

/// Panic unless no `.env` is in reach of the current directory.
pub fn assert_no_dotenv_reach() {
    let cwd = std::env::current_dir().unwrap();
    if let Err(refusal) = research_archive::env_guard::check(std::iter::empty(), &cwd) {
        panic!("refusing to run this test: {refusal}");
    }
}
