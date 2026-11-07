//! Execution-keyed output survives native process retirement. EOF is independent of scope drain.
use calm_session::control::execution::OutputEnd;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::fd::AsRawFd,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

pub(super) struct OutputLog {
    data: Mutex<Data>,
    wake: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    stop_reader: std::sync::atomic::AtomicBool,
    reader_end: tokio::sync::watch::Sender<Option<Result<(), String>>>,
}
struct Data {
    writer: Option<File>,
    path: PathBuf,
    cursor: u64,
    limit: usize,
}
impl OutputLog {
    pub fn open(path: &Path, limit: usize) -> anyhow::Result<Arc<Self>> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)?;
        Self::from_file(path, limit, file, true)
    }
    /// Restart validates existing history without creating or retaining a writable file.
    pub fn open_history(path: &Path, limit: usize) -> anyhow::Result<Arc<Self>> {
        Self::from_file(path, limit, Self::history_file(path)?, false)
    }
    fn history_file(path: &Path) -> std::io::Result<File> {
        use std::os::unix::fs::OpenOptionsExt;
        let file = std::fs::OpenOptions::new()
            .read(true)
            // A corrupt FIFO must be rejected, not block startup waiting for a writer.
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(path)?;
        if !file.metadata()?.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "historical output log is not a regular file",
            ));
        }
        Ok(file)
    }
    fn from_file(
        path: &Path,
        limit: usize,
        file: File,
        writable: bool,
    ) -> anyhow::Result<Arc<Self>> {
        let cursor = file.metadata()?.len();
        let (reader_end, _) = tokio::sync::watch::channel(None);
        Ok(Arc::new(Self {
            stop_reader: std::sync::atomic::AtomicBool::new(false),
            wake: Mutex::new(None),
            data: Mutex::new(Data {
                writer: writable.then_some(file),
                path: path.to_owned(),
                cursor,
                limit: limit.clamp(1024, 4 * 1024 * 1024),
            }),
            reader_end,
        }))
    }
    /// The owner calls this only after output completion has been persisted, under its scope gate.
    pub fn close_writer(&self) -> anyhow::Result<()> {
        self.data.lock().map_err(super::poison)?.writer.take();
        Ok(())
    }
    pub fn set_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self.wake.lock().expect("output wake") = Some(Box::new(wake));
    }
    pub fn snapshot(&self, from: Option<u64>) -> anyhow::Result<(u64, Vec<u8>)> {
        let mut data = self.data.lock().map_err(super::poison)?;
        let cursor = data.cursor;
        let start = cursor.saturating_sub(data.limit as u64);
        let from = from.unwrap_or(start).clamp(start, cursor);
        let mut history;
        let Data { writer, path, .. } = &mut *data;
        let file = match writer {
            Some(writer) => writer,
            None => {
                history = Self::history_file(path)?;
                &mut history
            }
        };
        file.seek(SeekFrom::Start(from))?;
        let mut bytes = vec![0; (cursor - from) as usize];
        file.read_exact(&mut bytes)?;
        Ok((cursor, bytes))
    }
    fn append(&self, bytes: &[u8]) -> anyhow::Result<()> {
        let mut data = self.data.lock().map_err(super::poison)?;
        let file = data
            .writer
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("output writer closed"))?;
        file.seek(SeekFrom::End(0))?;
        file.write_all(bytes)?;
        data.cursor += bytes.len() as u64;
        drop(data);
        if let Some(wake) = self.wake.lock().map_err(super::poison)?.as_ref() {
            wake();
        }
        Ok(())
    }
    pub fn read(self: &Arc<Self>, mut reader: File, pty: bool) {
        let log = self.clone();
        std::thread::spawn(move || {
            let mut bytes = [0; 8192];
            let mut failure = None;
            let result = loop {
                if log.stop_reader.load(std::sync::atomic::Ordering::SeqCst) {
                    break Err("output remained open beyond native completion budget".into());
                }
                match reader.read(&mut bytes) {
                    Ok(0) => break Ok(()),
                    Ok(count) => {
                        if failure.is_none()
                            && let Err(error) = log.append(&bytes[..count])
                        {
                            failure = Some(error.to_string());
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        let mut descriptor = libc::pollfd {
                            fd: reader.as_raw_fd(),
                            events: libc::POLLIN,
                            revents: 0,
                        };
                        // SAFETY: the thread owns this descriptor for the whole poll.
                        unsafe {
                            libc::poll(&mut descriptor, 1, 100);
                        }
                    }
                    Err(error) if pty && error.raw_os_error() == Some(libc::EIO) => break Ok(()),
                    Err(error) => break Err(error.to_string()),
                }
            };
            log.reader_end
                .send_replace(Some(failure.map_or(result, Err)));
        });
    }
    #[cfg(target_os = "macos")]
    pub async fn finish_bounded(&self, budget: std::time::Duration) -> OutputEnd {
        match tokio::time::timeout(budget, self.finish()).await {
            Ok(end) => end,
            Err(_) => {
                self.stop_reader
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                // The native reader is nonblocking and polls for at most 100ms.
                self.finish().await
            }
        }
    }
    /// Called after the platform's native reap; never while holding a scope gate.
    pub async fn finish(&self) -> OutputEnd {
        let mut end = self.reader_end.subscribe();
        let result = loop {
            if let Some(result) = end.borrow_and_update().clone() {
                break result;
            }
            if end.changed().await.is_err() {
                break Err("output reader disappeared".into());
            }
        };
        if let Err(reason) = result {
            return OutputEnd::Incomplete { reason };
        }
        match self.data.lock() {
            Ok(data) => match data
                .writer
                .as_ref()
                .ok_or_else(|| std::io::Error::other("output writer closed before completion"))
                .and_then(File::sync_all)
            {
                Ok(()) => OutputEnd::Complete {
                    final_cursor: data.cursor,
                },
                Err(error) => OutputEnd::Incomplete {
                    reason: error.to_string(),
                },
            },
            Err(_) => OutputEnd::Incomplete {
                reason: "output log poisoned".into(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[tokio::test]
    async fn native_macos_output_held_open_finishes_incomplete_within_budget() {
        let temp = tempfile::tempdir().unwrap();
        let log = OutputLog::open(&temp.path().join("log"), 1024).unwrap();
        // A private held writer reproduces missing EOF without creating an escaped child.
        let (reader, _writer) = std::os::unix::net::UnixStream::pair().unwrap();
        reader.set_nonblocking(true).unwrap();
        let fd: std::os::fd::OwnedFd = reader.into();
        log.read(File::from(fd), false);
        let end = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            log.finish_bounded(std::time::Duration::from_millis(10)),
        )
        .await
        .expect("native output reader must end even without EOF");
        assert!(matches!(end, OutputEnd::Incomplete { .. }), "{end:?}");
    }

    #[test]
    fn history_preserves_tail_cursor_and_propagates_read_errors() {
        let temp = tempfile::tempdir().unwrap();
        assert!(OutputLog::open_history(temp.path(), 1024).is_err());
        let path = temp.path().join("log");
        let log = OutputLog::open(&path, 1024).unwrap();
        log.append(&vec![b'a'; 2048]).unwrap();
        log.append(b"tail").unwrap();
        log.close_writer().unwrap();
        for log in [log, OutputLog::open_history(&path, 1024).unwrap()] {
            let (cursor, tail) = log.snapshot(None).unwrap();
            assert_eq!(cursor, 2052);
            assert_eq!(tail.len(), 1024);
            assert!(tail.ends_with(b"tail"));
            assert_eq!(log.snapshot(Some(0)).unwrap(), (cursor, tail));
            assert_eq!(
                log.snapshot(Some(2048)).unwrap(),
                (cursor, b"tail".to_vec())
            );
            assert_eq!(log.snapshot(Some(u64::MAX)).unwrap(), (cursor, Vec::new()));
        }
        let log = OutputLog::open_history(&path, 1024).unwrap();
        std::fs::write(&path, b"short").unwrap();
        assert!(log.snapshot(None).is_err());
        std::fs::remove_file(&path).unwrap();
        assert!(log.snapshot(None).is_err());
        assert!(OutputLog::open_history(&path, 1024).is_err());
        assert!(!path.exists());
        let other = temp.path().join("other");
        std::fs::write(&other, b"unrelated").unwrap();
        std::os::unix::fs::symlink(&other, &path).unwrap();
        assert!(OutputLog::open_history(&path, 1024).is_err());
        assert!(log.snapshot(None).is_err());
        assert_eq!(std::fs::read(&other).unwrap(), b"unrelated");
    }

    #[tokio::test]
    async fn output_read_error_is_incomplete() {
        let temp = tempfile::tempdir().unwrap();
        let log = OutputLog::open(&temp.path().join("log"), 1024).unwrap();
        // A directory's real read returns EISDIR, not EOF or the PTY-specific EIO.
        log.read(File::open(temp.path()).unwrap(), false);
        let mut end = log.reader_end.subscribe();
        end.wait_for(|value| value.is_some()).await.unwrap();
        assert!(matches!(log.finish().await, OutputEnd::Incomplete { .. }));
    }
}
