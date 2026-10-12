//! Bounded reads of operator-named files.
//!
//! Opening a FIFO for reading blocks until a writer appears, and a device
//! such as `/dev/zero` never ends, so a path a command reads (`--config`,
//! `auth import --from`, the stored token) must be a regular file of bounded
//! size. Symlinks are followed: a link to a regular file is fine.

use std::{
    fs::{self, File},
    io::{self, Read},
    path::Path,
};

/// The largest config or token file read.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;

/// Refuse a path that is not a regular file, without opening it.
pub fn require_regular(path: &Path) -> io::Result<()> {
    if fs::metadata(path)?.is_file() {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ))
    }
}

/// Read a regular file of at most [`MAX_FILE_BYTES`]. The type is checked
/// before the open (a FIFO would block it) and again on the opened file.
pub fn read_regular(path: &Path) -> io::Result<Vec<u8>> {
    require_regular(path)?;
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("larger than {MAX_FILE_BYTES} bytes"),
        ));
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_bounded_regular_files_are_read() {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("f");
        fs::write(&file, "x").unwrap();
        assert_eq!(read_regular(&file).unwrap(), b"x");
        let link = root.path().join("link");
        std::os::unix::fs::symlink(&file, &link).unwrap();
        assert_eq!(read_regular(&link).unwrap(), b"x");

        let fifo = root.path().join("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap();
        assert!(made.success());
        for path in [fifo.as_path(), root.path(), Path::new("/dev/zero")] {
            let err = read_regular(path).unwrap_err();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{path:?}");
        }

        fs::write(&file, vec![b'x'; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert_eq!(
            read_regular(&file).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
