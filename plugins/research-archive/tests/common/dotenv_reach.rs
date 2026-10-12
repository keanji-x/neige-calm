//! Shared by every test binary that builds an SDK `OAuth` or source in
//! process: the SDK's `HttpClientConfig::from_oauth` runs `dotenv()`, which
//! loads the first `.env` in the working directory or a parent into the
//! test process (adding `NO_PROXY`, `LONGBRIDGE_ENV`, …). Such a test runs
//! only where no `.env` is in reach.

use std::path::Path;

/// The first directory at or above `dir` holding a `.env` dotenv would load.
pub fn dotenv_above(dir: &Path) -> Option<std::path::PathBuf> {
    dir.ancestors()
        .find(|d| d.join(".env").is_file())
        .map(Path::to_owned)
}

/// Panic unless no `.env` is in reach of the current directory.
pub fn assert_no_dotenv_reach() {
    let cwd = std::env::current_dir().unwrap();
    if let Some(dir) = dotenv_above(&cwd) {
        panic!(
            "refusing to run: the SDK would load {}/.env into this test process",
            dir.display()
        );
    }
}
