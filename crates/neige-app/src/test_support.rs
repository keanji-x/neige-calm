//! Test-only helpers shared by every `#[cfg(test)]` module in this crate.

use std::ops::Deref;
use std::path::Path;

/// RAII temporary directory for tests; removed when the guard drops.
pub(crate) struct TestTempDir(tempfile::TempDir);

impl Deref for TestTempDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        self.0.path()
    }
}

/// Create `$TMPDIR/neige-app-<name>-<random>/`; a leftover directory still names the
/// test that produced it.
pub(crate) fn test_temp_dir(name: &str) -> TestTempDir {
    let dir = tempfile::Builder::new()
        .prefix(&format!("neige-app-{name}-"))
        .tempdir()
        .expect("create temp dir");
    TestTempDir(dir)
}
