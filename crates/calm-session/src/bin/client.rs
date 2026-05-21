//! calm-session-client — minimal standalone client for manual verification.
//!
//! Puts the local tty into raw mode, connects to a daemon socket, sends a
//! v2 [`ClientMsg::ClientHello`] with the terminal's current size, pumps
//! the local stdin to the daemon and the daemon's render-plane output to
//! local stdout. Exits cleanly on [`DaemonMsg::TerminalExited`] (terminal
//! mode), [`DaemonMsg::ChildExited`] (chat mode), or Ctrl+] (the typical
//! "emergency detach" — chosen because it isn't produced by any common
//! key chord that a shell or TUI needs).

use std::os::unix::io::AsRawFd;

use clap::Parser;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use calm_session::{
    ClientCapabilities, ClientMsg, DaemonMsg, InitialScrollback, PROTOCOL_VERSION, PtySize,
    RenderEncoding, read_frame, write_frame,
};
use uuid::Uuid;

#[derive(Parser, Debug)]
#[command(about = "Attach to a calm-session-daemon socket")]
struct Cli {
    /// Path of the daemon's Unix socket.
    sock: std::path::PathBuf,

    /// Terminal id the daemon was launched with. The daemon validates the
    /// `ClientHello.terminal_id` matches; mismatch closes the connection
    /// with `BadHandshake`.
    #[arg(long)]
    terminal_id: String,
}

fn term_size() -> (u16, u16) {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let fd = std::io::stdout().as_raw_fd();
    // SAFETY: ioctl is the standard way to read the window size, and
    // &mut ws is a correctly-sized winsize.
    unsafe {
        if libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) == 0 {
            (ws.ws_col, ws.ws_row)
        } else {
            (80, 24)
        }
    }
}

struct RawGuard {
    saved: nix::sys::termios::Termios,
}

impl RawGuard {
    fn enter() -> anyhow::Result<Self> {
        use nix::sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr};
        let stdin = std::io::stdin();
        let saved = tcgetattr(&stdin)?;
        let mut raw = saved.clone();
        cfmakeraw(&mut raw);
        tcsetattr(&stdin, SetArg::TCSANOW, &raw)?;
        Ok(Self { saved })
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        use nix::sys::termios::{SetArg, tcsetattr};
        let _ = tcsetattr(std::io::stdin(), SetArg::TCSANOW, &self.saved);
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let sock = UnixStream::connect(&cli.sock).await?;
    let (mut rd, mut wr) = sock.into_split();

    let (cols, rows) = term_size();
    write_frame(
        &mut wr,
        &ClientMsg::ClientHello {
            protocol_version: PROTOCOL_VERSION,
            terminal_id: cli.terminal_id.clone(),
            client_id: Uuid::new_v4(),
            desired_size: PtySize {
                cols,
                rows,
                pixel_width: None,
                pixel_height: None,
            },
            cell_size: None,
            initial_scrollback: InitialScrollback::None,
            resume_from: None,
            role_hint: None,
            capabilities: ClientCapabilities {
                render_encodings: vec![RenderEncoding::Vt],
                supports_scrollback: false,
                supports_sixel: false,
                supports_images: false,
                kernel_originated_input: false,
            },
        },
    )
    .await?;

    // Raw mode lasts for the life of this guard; it's restored on drop even
    // if we exit via an error path. Silently skip when stdin isn't a tty
    // (smoke tests, piped input) — the daemon still works, you just lose
    // line-editing suppression and Ctrl+] escape.
    let _raw = RawGuard::enter().ok();

    // Upstream: local stdin → ClientMsg::Input. Ctrl+] (0x1d) is our escape
    // hatch to force-detach, because the daemon won't close us otherwise.
    let up = tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            let n = match stdin.read(&mut buf).await {
                Ok(0) => {
                    // stdin EOF — hold the write half open so downstream
                    // stays live. Without this, piped/null stdin would
                    // detach the client the instant the attach returned.
                    std::future::pending::<()>().await;
                    return;
                }
                Ok(n) => n,
                Err(_) => break,
            };
            let bytes = &buf[..n];
            if bytes.contains(&0x1d) {
                break;
            }
            // Standalone CLI client doesn't need ack tracking; leave
            // input_seq at 0 ("no ack requested" — option (b) from
            // issue #115). The daemon will write the bytes and stay
            // silent.
            if write_frame(
                &mut wr,
                &ClientMsg::Input {
                    data: bytes.to_vec(),
                    input_seq: 0,
                },
            )
            .await
            .is_err()
            {
                break;
            }
        }
    });

    // Downstream: DaemonMsg → local stdout. ServerHello's snapshot.data
    // reproduces the current grid state verbatim; subsequent RenderPatches
    // are the live stream.
    let mut stdout = tokio::io::stdout();
    loop {
        let msg: DaemonMsg = match read_frame(&mut rd).await {
            Ok(m) => m,
            Err(_) => break,
        };
        match msg {
            DaemonMsg::ServerHello { snapshot, .. } => {
                stdout.write_all(&snapshot.data).await?;
                stdout.flush().await?;
            }
            DaemonMsg::HelloChat { replay } => {
                for ev in &replay {
                    stdout.write_all(ev.as_bytes()).await?;
                    stdout.write_all(b"\n").await?;
                }
                stdout.flush().await?;
            }
            DaemonMsg::RenderSnapshot(snap) => {
                stdout.write_all(&snap.data).await?;
                stdout.flush().await?;
            }
            DaemonMsg::RenderPatch(p) => {
                stdout.write_all(&p.data).await?;
                stdout.flush().await?;
            }
            DaemonMsg::ChatEvent { json } => {
                stdout.write_all(json.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
            DaemonMsg::TerminalExited { code, .. } => {
                eprintln!("\r\n[session ended, code={code:?}]");
                break;
            }
            DaemonMsg::ChildExited { code } => {
                eprintln!("\r\n[chat session ended, code={code:?}]");
                break;
            }
            DaemonMsg::ProtocolError {
                code,
                message,
                expected_version,
            } => {
                eprintln!(
                    "\r\n[protocol error: code={code:?} message={message:?} expected_version={expected_version:?}]"
                );
                break;
            }
            DaemonMsg::ResizeApplied { .. }
            | DaemonMsg::OwnerChanged { .. }
            | DaemonMsg::Backpressure { .. }
            | DaemonMsg::SnapshotRequired { .. }
            | DaemonMsg::ChildReady { .. }
            | DaemonMsg::InputAck { .. } => {
                // Informational frames the minimal CLI doesn't surface.
                // `InputAck` never fires for this CLI because every
                // Input frame it sends carries `input_seq: 0` ("no ack
                // requested" — option (b)); the arm is here to keep
                // `match msg` exhaustive.
            }
        }
    }

    up.abort();
    Ok(())
}
