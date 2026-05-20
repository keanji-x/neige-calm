//! calm-session-client — minimal standalone client for manual verification.
//!
//! Puts the local tty into raw mode, connects to a daemon socket, sends
//! Attach with the terminal's current size, pumps the terminal's stdin to the
//! daemon and the daemon's stdout to the terminal. Exits cleanly on
//! ChildExited or Ctrl+] (the typical "emergency detach" — chosen because it
//! isn't produced by any common key chord that a shell or TUI needs).

use std::os::unix::io::AsRawFd;

use clap::Parser;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

use calm_session::{ClientMsg, DaemonMsg, read_frame, write_frame};

#[derive(Parser, Debug)]
#[command(about = "Attach to a calm-session-daemon socket")]
struct Cli {
    /// Path of the daemon's Unix socket.
    sock: std::path::PathBuf,
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
    write_frame(&mut wr, &ClientMsg::Attach { cols, rows }).await?;

    // Raw mode lasts for the life of this guard; it's restored on drop even
    // if we exit via an error path. Silently skip when stdin isn't a tty
    // (smoke tests, piped input) — the daemon still works, you just lose
    // line-editing suppression and Ctrl+] escape.
    let _raw = RawGuard::enter().ok();

    // Upstream: local stdin → ClientMsg::Stdin. Ctrl+] (0x1d) is our escape
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
            if write_frame(&mut wr, &ClientMsg::Stdin(bytes.to_vec()))
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // Downstream: DaemonMsg → local stdout (including the initial Hello
    // replay, which reproduces the current grid state verbatim).
    let mut stdout = tokio::io::stdout();
    loop {
        let msg: DaemonMsg = match read_frame(&mut rd).await {
            Ok(m) => m,
            Err(_) => break,
        };
        match msg {
            DaemonMsg::Hello { replay } => {
                stdout.write_all(&replay).await?;
                stdout.flush().await?;
            }
            DaemonMsg::HelloChat { replay } => {
                for ev in &replay {
                    stdout.write_all(ev.as_bytes()).await?;
                    stdout.write_all(b"\n").await?;
                }
                stdout.flush().await?;
            }
            DaemonMsg::Stdout(b) => {
                stdout.write_all(&b).await?;
                stdout.flush().await?;
            }
            DaemonMsg::ChatEvent { json } => {
                stdout.write_all(json.as_bytes()).await?;
                stdout.write_all(b"\n").await?;
                stdout.flush().await?;
            }
            DaemonMsg::ChildExited { code } => {
                eprintln!("\r\n[session ended, code={code:?}]");
                break;
            }
        }
    }

    up.abort();
    Ok(())
}
