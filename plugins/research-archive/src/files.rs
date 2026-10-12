//! Bounded reads of operator-named files.
//!
//! Opening a FIFO for reading blocks until a writer appears, and a device
//! such as `/dev/zero` never ends, so a path a command reads (`--config`,
//! `auth import --from`, the stored token) must be a regular file of bounded
//! size. Symlinks are followed: a link to a regular file is fine.
//!
//! The file is opened with `O_NONBLOCK`, so even a path swapped to a FIFO
//! after any earlier check opens at once, and its type is then checked on
//! the open descriptor (`fstat`). On a regular file `O_NONBLOCK` has no
//! effect.

use std::{
    fs::{self, OpenOptions},
    io::{self, Read},
    os::unix::fs::OpenOptionsExt,
    path::Path,
};

/// The largest config or token file read.
pub const MAX_FILE_BYTES: u64 = 64 * 1024;

/// Refuse a path that is not a regular file, without opening it. A fast,
/// early check (`auth import` runs it before taking the lock);
/// [`read_regular`] does not rely on it.
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

/// Read a regular file of at most [`MAX_FILE_BYTES`]: opened without
/// blocking, then its type checked on the open descriptor.
pub fn read_regular(path: &Path) -> io::Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)?;
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

        // Opening a FIFO with no writer never blocks.
        let (tx, rx) = std::sync::mpsc::channel();
        let path = fifo.clone();
        std::thread::spawn(move || tx.send(read_regular(&path).map_err(|e| e.kind())));
        assert_eq!(
            rx.recv_timeout(std::time::Duration::from_secs(5)),
            Ok(Err(io::ErrorKind::InvalidInput))
        );

        fs::write(&file, vec![b'x'; MAX_FILE_BYTES as usize + 1]).unwrap();
        assert_eq!(
            read_regular(&file).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }
}
