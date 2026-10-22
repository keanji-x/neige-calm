//! Bounded stderr capture. Missing completion evidence never means untruncated.
use crate::error::Result;
use std::path::{Path, PathBuf};

pub const CAP: usize = 262144;
#[derive(Debug, PartialEq, Eq)]
pub enum DroppedBytes {
    Exact(u64),
    AtLeast(u64),
}
#[derive(Debug, PartialEq, Eq)]
pub enum Capture {
    Complete,
    StderrTruncated { dropped_bytes: DroppedBytes },
    StderrCaptureInterrupted,
    StderrUnreadable,
}
pub struct Diagnostic {
    pub bytes: Vec<u8>,
    pub capture: Capture,
}

pub fn directory(root: &Path, id: &str) -> PathBuf {
    root.join(id)
}
pub fn create(root: &Path, id: &str) -> Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    let dir = directory(root, id);
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    Ok(dir)
}
/// Arguments are passed as positional parameters, never interpolated into shell source.
pub fn wrapper(release_prefix: &str) -> String {
    format!(
        r#"umask 077
cd "$1" || exit 75
mkfifo err.fifo || exit 75
{{
    head -c {CAP} >stderr
    if [ "$(dd bs=1 count=1 2>/dev/null | wc -c)" -gt 0 ]; then
        : >stderr.overflow
        wc -c >stderr.dropped.tmp && mv stderr.dropped.tmp stderr.dropped
    fi
    : >stderr.closed
}} <err.fifo &
exec 2>err.fifo || exit 75
rm err.fifo || exit 75
cd "$2" || exit 75
shift 2
{release_prefix}exec "$@"
"#
    )
}
fn read(dir: &Path) -> std::io::Result<Diagnostic> {
    let bytes = std::fs::read(dir.join("stderr"))?;
    let exists = |name: &str| -> std::io::Result<bool> { dir.join(name).try_exists() };
    let capture = if exists("stderr.overflow")? {
        let count = std::fs::read_to_string(dir.join("stderr.dropped"))
            .ok()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .and_then(|n| n.checked_add(1));
        Capture::StderrTruncated {
            dropped_bytes: count
                .map(DroppedBytes::Exact)
                .unwrap_or(DroppedBytes::AtLeast(1)),
        }
    } else if exists("stderr.closed")? {
        Capture::Complete
    } else {
        Capture::StderrCaptureInterrupted
    };
    Ok(Diagnostic { bytes, capture })
}
pub fn diagnostic(dir: &Path) -> Diagnostic {
    read(dir).unwrap_or_else(|error| {
        tracing::warn!(path=%dir.display(), %error, "stderr diagnostic unreadable");
        Diagnostic {
            bytes: vec![],
            capture: Capture::StderrUnreadable,
        }
    })
}
pub fn cleanup(root: &Path, id: &str) {
    // ExecutionId validation belongs at admission/read boundaries, before paths are used.
    if let Err(error) = std::fs::remove_dir_all(directory(root, id))
        && error.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!(%id, %error, "could not remove provider diagnostic directory");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn c4_stderr_missing_closed_never_means_complete() {
        let temp = tempfile::tempdir().unwrap();
        std::fs::write(temp.path().join("stderr"), b"kept").unwrap();
        assert_eq!(
            diagnostic(temp.path()).capture,
            Capture::StderrCaptureInterrupted
        );
        std::fs::write(temp.path().join("stderr.overflow"), b"").unwrap();
        assert_eq!(
            diagnostic(temp.path()).capture,
            Capture::StderrTruncated {
                dropped_bytes: DroppedBytes::AtLeast(1)
            }
        );
    }
}
