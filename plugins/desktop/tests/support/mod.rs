//! Drives the real `desktop` binary over its stdio MCP and `http.sock`, with
//! the fake Chrome fixture as its `chrome_binary`.
#![allow(dead_code)]

use std::collections::{BTreeSet, HashMap};
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

pub const FAKE_CHROME: &str = env!("CARGO_BIN_EXE_desktop-fake-chrome");
pub const DESKTOP: &str = env!("CARGO_BIN_EXE_desktop");
pub const SIZE: (u32, u32) = (640, 480);
/// The `bwrap_binary` the manifest defaults to.
pub const BWRAP: &str = "/usr/bin/bwrap";
/// The fixture's colours, as 0xRRGGBB.
pub const BLUE: u32 = 0x2050a0;
pub const RED: u32 = 0xc03020;

/// A temporary directory under `TMPDIR`, short enough for `sun_path`.
pub fn tmp() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("dsk")
        .tempdir()
        .expect("tmp dir")
}

pub struct Plugin {
    pub child: Child,
    stdin: Option<ChildStdin>,
    replies: Receiver<Value>,
    /// Ends the stdout reader, which then closes the read end.
    stop_reading: Arc<AtomicBool>,
    pending: HashMap<u64, Value>,
    next_id: u64,
    pub data: PathBuf,
}

impl Plugin {
    /// Spawns `desktop` the way the plugin host does: cwd and
    /// `NEIGE_PLUGIN_DATA_DIR` are the work dir, which already exists.
    pub fn spawn(data: &Path) -> Self {
        Self::spawn_with(data, Stdio::inherit(), "debug", &[])
    }

    /// With `env` added to the environment the test runs in (Chrome keeps
    /// `LANG` and `LC_*` from it).
    pub fn spawn_env(data: &Path, env: &[(&str, &str)]) -> Self {
        Self::spawn_with(data, Stdio::inherit(), "debug", env)
    }

    /// With stderr on a pipe the test holds, as the plugin host does: dropping
    /// [`Plugin::stderr`] closes it like a kernel that died.
    pub fn spawn_kernel_like(data: &Path) -> Self {
        Self::spawn_with(data, Stdio::piped(), "info", &[])
    }

    fn spawn_with(data: &Path, stderr: Stdio, log: &str, env: &[(&str, &str)]) -> Self {
        std::fs::create_dir_all(data).unwrap();
        let mut child = Command::new(DESKTOP)
            .current_dir(data)
            .env("NEIGE_PLUGIN_ID", "desktop")
            .env("NEIGE_PLUGIN_DATA_DIR", data)
            .env("NEIGE_PLUGIN_TOKEN", "tok-123")
            .env("DESKTOP_LOG", log)
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .expect("spawn desktop");
        let stdout = child.stdout.take().unwrap();
        let (tx, replies) = channel();
        let stop_reading = Arc::new(AtomicBool::new(false));
        let stop = stop_reading.clone();
        std::thread::spawn(move || read_frames(stdout, &tx, &stop));
        Self {
            stdin: child.stdin.take(),
            child,
            replies,
            stop_reading,
            pending: HashMap::new(),
            next_id: 1,
            data: data.to_path_buf(),
        }
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn home(&self) -> PathBuf {
        self.data.join("home")
    }

    /// Sends one request and waits for its reply frame.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        self.reply(id)
    }

    /// Sends one request without waiting; its id.
    pub fn send(&mut self, method: &str, params: Value) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        self.write_line(frame.to_string().as_bytes());
        id
    }

    /// Writes `bytes` and a newline to stdin as they are.
    pub fn write_line(&mut self, bytes: &[u8]) {
        let stdin = self.stdin.as_mut().expect("stdin open");
        stdin.write_all(bytes).unwrap();
        stdin.write_all(b"\n").unwrap();
        stdin.flush().unwrap();
    }

    /// Waits for the reply to request `id`.
    pub fn reply(&mut self, id: u64) -> Value {
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            if let Some(reply) = self.pending.remove(&id) {
                return reply;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            let reply = self.replies.recv_timeout(left).expect("a reply in time");
            let rid = reply["id"].as_u64().expect("reply id");
            self.pending.insert(rid, reply);
        }
    }

    pub fn initialize(&mut self, chrome: &str) -> Value {
        self.initialize_with(chrome, BWRAP)
    }

    /// `initialize` with the defaults the kernel merges in, and `bwrap`.
    pub fn initialize_with(&mut self, chrome: &str, bwrap: &str) -> Value {
        self.request(
            "initialize",
            json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" },
                "_meta": {
                    "dev.neige/auth": { "expected_echo": "tok-123" },
                    "dev.neige/config": { "values": {
                        "chrome_binary": chrome, "bwrap_binary": bwrap,
                        "width": SIZE.0, "height": SIZE.1, "max_fps": 15 } }
                }
            }),
        )
    }

    /// `tools/call`: `Ok(result)` or `Err(error object)`.
    pub fn tool(&mut self, name: &str, arguments: Value) -> Result<Value, Value> {
        let reply = self.request(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        match reply.get("error") {
            Some(error) => Err(error.clone()),
            None => Ok(reply["result"].clone()),
        }
    }

    pub fn close_stdin(&mut self) {
        self.stdin.take();
    }

    /// The kernel side of every pipe goes away at once, as when the kernel
    /// dies: stdin reads EOF, and writes to stdout and stderr fail.
    pub fn kernel_gone(&mut self) {
        self.stdin.take();
        self.child.stderr.take();
        self.stop_reading.store(true, Ordering::SeqCst);
    }

    /// Waits for the process to exit; its status, `None` if it still runs.
    pub fn wait_exit(&mut self, within: Duration) -> Option<std::process::ExitStatus> {
        let deadline = Instant::now() + within;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return Some(status);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The fake Chrome's pid as it sees it, once it has started: inside its
    /// sandbox, a pid of the sandbox's pid namespace.
    pub fn fake_inner_pid(&self) -> u32 {
        let file = self.home().join("fake-chrome.pid");
        wait_for(Duration::from_secs(10), || {
            std::fs::read_to_string(&file).is_ok_and(|pid| pid.parse::<u32>().is_ok())
        });
        std::fs::read_to_string(file).unwrap().parse().unwrap()
    }

    /// The fake Chrome's pid on the host, once it has started: the process
    /// below this `desktop` whose innermost pid is the one the fake wrote.
    pub fn fake_pid(&self) -> u32 {
        let inner = self.fake_inner_pid();
        let mut found = None;
        wait_for(Duration::from_secs(10), || {
            found = descendants(self.pid())
                .into_iter()
                .find(|&pid| innermost_pid(pid) == Some(inner));
            found.is_some()
        });
        found.unwrap()
    }

    /// Makes the fake Chrome close its window and exit.
    pub fn close_fake_window(&self) {
        std::fs::write(self.home().join("close"), b"").unwrap();
    }

    pub fn fake_log(&self) -> Vec<String> {
        std::fs::read_to_string(self.home().join("fake-chrome.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    pub fn socket(&self) -> PathBuf {
        self.data.join("http.sock")
    }
}

impl Drop for Plugin {
    fn drop(&mut self) {
        // The test's own child: SIGKILL, like the kernel's plugin stop.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Forwards JSON lines from `stdout` until EOF or `stop`, then drops it.
fn read_frames(mut stdout: ChildStdout, tx: &Sender<Value>, stop: &AtomicBool) {
    let mut pending = Vec::new();
    let mut chunk = [0u8; 65536];
    loop {
        let mut fd = libc::pollfd {
            fd: stdout.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let ready = unsafe { libc::poll(&mut fd, 1, 50) };
        if stop.load(Ordering::SeqCst) {
            return;
        }
        if ready <= 0 {
            continue;
        }
        let n = match stdout.read(&mut chunk) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        pending.extend_from_slice(&chunk[..n]);
        while let Some(end) = pending.iter().position(|&b| b == b'\n') {
            let frame: Value =
                serde_json::from_slice(&pending[..end]).expect("desktop writes JSON lines");
            pending.drain(..=end);
            if tx.send(frame).is_err() {
                return;
            }
        }
    }
}

/// Starts the fake Chrome through `page_open`, which the fake cannot serve;
/// its host pid.
pub fn launch_fake(plugin: &mut Plugin) -> u32 {
    let err = plugin
        .tool("page_open", json!({ "url": "https://example.com/" }))
        .unwrap_err();
    assert_eq!(err["code"], -32409, "{err}");
    let fake = plugin.fake_pid();
    assert!(alive(fake));
    fake
}

/// `pid`'s pid in its innermost pid namespace (`NSpid`'s last field).
pub fn innermost_pid(pid: u32) -> Option<u32> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let line = status.lines().find_map(|l| l.strip_prefix("NSpid:"))?;
    line.split_whitespace().last()?.parse().ok()
}

/// Every live descendant of `root`, by parent links.
pub fn descendants(root: u32) -> Vec<u32> {
    let parents: Vec<(u32, u32)> = std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .filter_map(|e| e.file_name().to_str()?.parse::<u32>().ok())
        .filter_map(|pid| {
            let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
            let ppid = stat
                .rsplit_once(')')?
                .1
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()?;
            Some((pid, ppid))
        })
        .collect();
    let mut found = Vec::new();
    let mut frontier = vec![root];
    while let Some(parent) = frontier.pop() {
        for &(pid, ppid) in &parents {
            if ppid == parent {
                found.push(pid);
                frontier.push(pid);
            }
        }
    }
    found
}

pub fn wait_for(within: Duration, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + within;
    while !done() {
        assert!(
            Instant::now() < deadline,
            "condition not met within {within:?}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Whether `pid` names a live (not reaped, not zombie) process.
pub fn alive(pid: u32) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Err(_) => false,
        Ok(stat) => {
            let state = stat.rsplit(')').next().unwrap_or("").trim_start();
            !state.starts_with('Z')
        }
    }
}

pub fn mode(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// Sends one HTTP/1.1 request on `socket`; the status code.
pub fn http_status(socket: &Path, request: &str) -> u16 {
    let mut stream = UnixStream::connect(socket).unwrap();
    stream.write_all(request.as_bytes()).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut head = [0u8; 12];
    stream.read_exact(&mut head).unwrap();
    std::str::from_utf8(&head[9..12]).unwrap().parse().unwrap()
}

pub fn upgrade_request(path: &str) -> String {
    format!(
        "GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\n\
         Sec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    )
}

/// The inodes of TCP sockets in LISTEN state, IPv4 and IPv6.
fn tcp_listen_inodes() -> BTreeSet<u64> {
    let mut inodes = BTreeSet::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(text) = std::fs::read_to_string(table) else {
            continue;
        };
        for line in text.lines().skip(1) {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // st (field 3) 0A is LISTEN; inode is field 9.
            if fields.get(3) == Some(&"0A")
                && let Some(inode) = fields.get(9).and_then(|i| i.parse().ok())
            {
                inodes.insert(inode);
            }
        }
    }
    inodes
}

/// TCP listening sockets that `pid` holds open.
pub fn tcp_listeners(pid: u32) -> Vec<u64> {
    let listening = tcp_listen_inodes();
    let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
        return Vec::new();
    };
    fds.flatten()
        .filter_map(|fd| std::fs::read_link(fd.path()).ok())
        .filter_map(|target| {
            let target = target.to_string_lossy().into_owned();
            target
                .strip_prefix("socket:[")?
                .strip_suffix(']')?
                .parse::<u64>()
                .ok()
        })
        .filter(|inode| listening.contains(inode))
        .collect()
}

/// The 0xRRGGBB colour at the centre of an RGB image.
pub fn centre_rgb(rgb: &[u8], width: usize, height: usize) -> u32 {
    let at = ((height / 2) * width + width / 2) * 3;
    u32::from_be_bytes([0, rgb[at], rgb[at + 1], rgb[at + 2]])
}

/// Decodes a base64 PNG to (width, height, RGB bytes).
pub fn decode_png(base64_png: &str) -> (usize, usize, Vec<u8>) {
    let bytes = base64_decode(base64_png);
    let decoder = png::Decoder::new(std::io::Cursor::new(bytes));
    let mut reader = decoder.read_info().unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let info = reader.next_frame(&mut buf).unwrap();
    assert_eq!(info.color_type, png::ColorType::Rgb);
    buf.truncate(info.buffer_size());
    (info.width as usize, info.height as usize, buf)
}

fn base64_decode(text: &str) -> Vec<u8> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(text)
        .unwrap()
}

/// Whether two colours are within `tolerance` per channel (JPEG is lossy).
pub fn near(a: u32, b: u32, tolerance: i32) -> bool {
    (0..3).all(|i| {
        let ca = ((a >> (i * 8)) & 0xff) as i32;
        let cb = ((b >> (i * 8)) & 0xff) as i32;
        (ca - cb).abs() <= tolerance
    })
}
