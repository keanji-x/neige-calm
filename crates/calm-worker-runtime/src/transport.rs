//! Bounded, raw stdio forwarding. No protocol interpretation or fabricated replay.
use crate::{Result, linux};
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::process::{ChildStdin, ChildStdout};

const BUFFER_LIMIT: usize = 262_144;

pub(crate) struct Transport {
    listener: UnixListener,
    client: Option<UnixStream>,
    input: ChildStdin,
    output: ChildStdout,
    to_provider: VecDeque<u8>,
    to_client: VecDeque<u8>,
    output_eof: bool,
    input_closed: bool,
}
impl Transport {
    pub fn new(directory: &Path, input: ChildStdin, output: ChildStdout) -> Result<Self> {
        let (_fd, path) = linux::socket_path(directory)?;
        let listener = UnixListener::bind(path)?;
        listener.set_nonblocking(true)?;
        linux::nonblocking(input.as_raw_fd())?;
        linux::nonblocking(output.as_raw_fd())?;
        Ok(Self {
            listener,
            client: None,
            input,
            output,
            to_provider: VecDeque::new(),
            to_client: VecDeque::new(),
            output_eof: false,
            input_closed: false,
        })
    }
    pub fn step(&mut self) -> Result<()> {
        // Read the old connection before admitting its replacement.
        if let Some(client) = &mut self.client {
            match read_available(client, &mut self.to_provider) {
                Ok(true) => self.client = None,
                Err(error) if connection_ended(&error) => self.client = None,
                Err(error) => return Err(error.into()),
                Ok(false) => {}
            }
        }
        if let Ok((client, _)) = self.listener.accept() {
            client.set_nonblocking(true)?;
            if self.client.is_none() {
                self.client = Some(client);
            }
            // An additional live client is closed; bytes are never interleaved.
        }
        if !self.input_closed
            && let Err(error) = write_available(&mut self.input, &mut self.to_provider)
        {
            if connection_ended(&error) {
                self.input_closed = true;
            } else {
                return Err(error.into());
            }
        }
        if self.input_closed {
            self.to_provider.clear();
        }
        if !self.output_eof {
            self.output_eof = read_available(&mut self.output, &mut self.to_client)?;
        }
        if let Some(client) = &mut self.client
            && let Err(error) = write_available(client, &mut self.to_client)
        {
            if connection_ended(&error) {
                self.client = None;
            } else {
                return Err(error.into());
            }
        }
        Ok(())
    }

    /// Quiescence and draining stdout are separate. Give an attached client a
    /// bounded chance to read; retain any undelivered tail outside Worker mounts.
    pub fn finish(&mut self, directory: &Path) -> Result<()> {
        self.input_closed = true;
        let until = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            self.step()?;
            if self.output_eof && self.to_client.is_empty() {
                return Ok(());
            }
            if self.client.is_none() || std::time::Instant::now() >= until {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let mut tail = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(directory.join("undelivered.stdout"))?;
        loop {
            let (front, back) = self.to_client.as_slices();
            tail.write_all(front)?;
            tail.write_all(back)?;
            self.to_client.clear();
            if self.output_eof {
                break;
            }
            self.output_eof = read_available(&mut self.output, &mut self.to_client)?;
            if self.to_client.is_empty() && !self.output_eof {
                return Err(crate::Error::Evidence(
                    "stdout remained open after init exit".into(),
                ));
            }
        }
        tail.sync_all()?;
        Ok(())
    }
}

fn connection_ended(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::NotConnected
    )
}
fn read_available(reader: &mut impl Read, queue: &mut VecDeque<u8>) -> std::io::Result<bool> {
    let mut bytes = [0u8; 8192];
    let length = bytes.len().min(BUFFER_LIMIT - queue.len());
    if length == 0 {
        return Ok(false);
    }
    match reader.read(&mut bytes[..length]) {
        Ok(0) => Ok(true),
        Ok(count) => {
            queue.extend(&bytes[..count]);
            Ok(false)
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}
fn write_available(writer: &mut impl Write, queue: &mut VecDeque<u8>) -> std::io::Result<()> {
    if queue.is_empty() {
        return Ok(());
    }
    let (front, _) = queue.as_slices();
    match writer.write(front) {
        Ok(0) => Err(std::io::ErrorKind::BrokenPipe.into()),
        Ok(count) => {
            queue.drain(..count);
            Ok(())
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
            ) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}
