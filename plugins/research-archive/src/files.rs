//! Bounded reads of operator-named files, and the one atomic whole-file
//! write ([`replace`]) every file the archive writes besides the database
//! goes through: the token, the calendar cache, `last_exit.json` and
//! `notify_state.json`.
//!
//! **Bounded reads.**
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
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
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
    read_regular_at_most(path, MAX_FILE_BYTES)
}

/// [`read_regular`] with its own size bound, for a file that grows (the
/// calendar cache).
pub fn read_regular_at_most(path: &Path, max_bytes: u64) -> io::Result<Vec<u8>> {
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
    file.take(max_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("larger than {max_bytes} bytes"),
        ));
    }
    Ok(bytes)
}

/// How many temp-file names one [`replace`] tries before giving up.
const TEMP_ATTEMPTS: usize = 16;

/// 128 random bits from the OS, as hex: a temp-file suffix no other process
/// can predict.
pub fn random_suffix() -> String {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).expect("the OS random source is available");
    hex::encode(bytes)
}

/// Why [`replace`] wrote nothing, with the path the failing step used.
#[derive(Debug, thiserror::Error)]
#[error("{path}: {source}")]
pub struct ReplaceError {
    pub path: PathBuf,
    pub source: io::Error,
}

/// Replace `target` with `bytes`, whole: a temp file `.<name>.tmp-<128 random
/// bits>` created with `create_new` and mode 0600 in the target's
/// directory, written and fsynced, renamed over `target`, then the directory
/// fsynced. The directory must exist. A reader sees the old file or the new
/// one, never a torn write.
pub fn replace(target: &Path, bytes: &[u8]) -> Result<(), ReplaceError> {
    replace_with_temp_names(target, bytes, std::iter::repeat_with(random_suffix))
}

/// [`replace`] with the temp-name suffixes drawn from `suffixes`, so a test
/// can make one collide.
///
/// A name that exists already is skipped, never touched: only a temp file
/// this call created is renamed or removed.
pub fn replace_with_temp_names(
    target: &Path,
    bytes: &[u8],
    suffixes: impl IntoIterator<Item = String>,
) -> Result<(), ReplaceError> {
    let err = |path: &Path| {
        let path = path.to_owned();
        move |source| ReplaceError { path, source }
    };
    let invalid = |what: &str| ReplaceError {
        path: target.to_owned(),
        source: io::Error::new(io::ErrorKind::InvalidInput, what.to_owned()),
    };
    let dir = target.parent().ok_or_else(|| invalid("no directory"))?;
    let name = target.file_name().ok_or_else(|| invalid("no file name"))?;
    let mut suffixes = suffixes.into_iter().take(TEMP_ATTEMPTS);
    let (temp, mut file) = loop {
        let Some(suffix) = suffixes.next() else {
            return Err(ReplaceError {
                path: dir.to_owned(),
                source: io::Error::new(io::ErrorKind::AlreadyExists, "no free temp-file name"),
            });
        };
        let temp = dir.join(format!(".{}.tmp-{suffix}", name.to_string_lossy()));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temp)
        {
            Ok(file) => break (temp, file),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            // Nothing was created, so there is nothing to remove.
            Err(e) => return Err(err(&temp)(e)),
        }
    };
    // From here on `temp` is ours: it is renamed into place or removed.
    let written = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(err(&temp))
        .and_then(|()| fs::rename(&temp, target).map_err(err(target)));
    if let Err(e) = written {
        let _ = fs::remove_file(&temp);
        return Err(e);
    }
    File::open(dir)
        .and_then(|dir| dir.sync_all())
        .map_err(err(dir))
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

    #[test]
    fn replace_writes_whole_0600_files_and_skips_taken_names() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let target = root.path().join("state.json");
        replace(&target, b"one").unwrap();
        // The first name is taken by someone else: it is skipped and left.
        let taken = root.path().join(".state.json.tmp-a");
        fs::write(&taken, "theirs").unwrap();
        replace_with_temp_names(&target, b"two", ["a".to_owned(), "b".to_owned()]).unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"two");
        assert_eq!(fs::read(&taken).unwrap(), b"theirs");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let err = replace_with_temp_names(&target, b"three", ["a".to_owned()]).unwrap_err();
        assert_eq!(err.source.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&target).unwrap(), b"two");
        let mut names: Vec<_> = fs::read_dir(root.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, [".state.json.tmp-a", "state.json"]);
    }
}
