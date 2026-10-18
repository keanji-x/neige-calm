//! Execution-keyed output survives native process retirement. EOF is independent of scope drain.
use calm_session::control::execution::OutputEnd;
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::fd::AsRawFd,
    path::Path,
    sync::{Arc, Mutex},
};

pub(super) struct OutputLog {
    data: Mutex<Data>,
    wake: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    reader_end: tokio::sync::watch::Sender<Option<Result<(), String>>>,
}
struct Data {
    file: File,
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
        let cursor = file.metadata()?.len();
        let (reader_end, _) = tokio::sync::watch::channel(None);
        Ok(Arc::new(Self {
            wake: Mutex::new(None),
            data: Mutex::new(Data {
                file,
                cursor,
                limit: limit.clamp(1024, 4 * 1024 * 1024),
            }),
            reader_end,
        }))
    }
    pub fn set_wake(&self, wake: impl Fn() + Send + Sync + 'static) {
        *self.wake.lock().expect("output wake") = Some(Box::new(wake));
    }
    pub fn snapshot(&self, from: Option<u64>) -> anyhow::Result<(u64, Vec<u8>)> {
        let mut data = self.data.lock().map_err(super::poison)?;
        let cursor = data.cursor;
        let start = cursor.saturating_sub(data.limit as u64);
        let from = from.unwrap_or(start).clamp(start, cursor);
        data.file.seek(SeekFrom::Start(from))?;
        let mut bytes = vec![0; (cursor - from) as usize];
        data.file.read_exact(&mut bytes)?;
        Ok((cursor, bytes))
    }
    fn append(&self, bytes: &[u8]) -> anyhow::Result<()> {
        let mut data = self.data.lock().map_err(super::poison)?;
        data.file.seek(SeekFrom::End(0))?;
        data.file.write_all(bytes)?;
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
    /// Called only after persisted Drained and helper reap; never while holding a scope gate.
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
            Ok(data) => match data.file.sync_all() {
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
