//! Test-only helpers shared by every `#[cfg(test)]` module in this crate.
//!
//! #1637 — nine modules each carried an identical `test_temp_dir` that
//! created `$TMPDIR/neige-app-<name>-<pid>` and never removed it, so every
//! CI run left one directory per test under the self-hosted runner's
//! `RUNNER_TEMP`. This is the single implementation: the returned guard
//! removes the directory when it drops, including during a panic unwind.

use std::ops::Deref;
use std::path::Path;

/// RAII temporary directory for tests. Derefs to [`Path`], so call sites
/// `tmp.join(..)` exactly as they did with the `PathBuf` the old helper
/// returned; the directory is removed when the guard drops.
pub(crate) struct TestTempDir(tempfile::TempDir);

impl Deref for TestTempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        self.0.path()
    }
}

/// Create `$TMPDIR/neige-app-<name>-<random>/`. The `neige-app-<name>-`
/// prefix is kept on purpose: a leftover directory still names the test
/// that produced it.
pub(crate) fn test_temp_dir(name: &str) -> TestTempDir {
    let dir = tempfile::Builder::new()
        .prefix(&format!("neige-app-{name}-"))
        .tempdir()
        .expect("create temp dir");
    TestTempDir(dir)
}
