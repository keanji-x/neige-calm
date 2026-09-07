//! Thin independent host for the pinned RMUX server library.
#![forbid(unsafe_code)]

use clap::Parser;
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "Neige private terminal runtime")]
struct Args {
    /// Absolute socket in an existing private directory. No default/global endpoint.
    #[arg(long)]
    socket: PathBuf,
}

fn main() -> anyhow::Result<()> {
    // Upstream's documented embedding hook handles its private FIFO reader
    // re-exec before normal CLI parsing. It never loads user configuration.
    #[cfg(unix)]
    if let Some(code) = rmux_server::run_internal_fifo_reader_helper(std::env::args_os().skip(1)) {
        std::process::exit(code);
    }
    let args = Args::parse();
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?
        .block_on(calm_terminal_runtime::serve(args.socket))?;
    Ok(())
}
