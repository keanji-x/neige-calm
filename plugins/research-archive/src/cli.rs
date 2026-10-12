//! The `research-archive` command line. [`run`] is the whole process: it
//! checks the environment, reads the config, pins the region while the
//! process is still single-threaded, then runs one command and returns its
//! exit code.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};

use crate::auth::{self, DataDirTokenStorage, TokenSummary};
use crate::config::Config;
use crate::env_guard;
use crate::exit;
use crate::lock::{DataDirLock, LockError};
use crate::logging;

#[derive(Debug, Parser)]
#[command(
    name = "research-archive",
    about = "Point-in-time archive of Longbridge fundamentals (#2575)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// The archive's own Longbridge token.
    #[command(subcommand)]
    Auth(AuthCommand),
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// Import a token file the SDK wrote for the configured client id.
    Import {
        #[arg(long)]
        config: PathBuf,
        /// The SDK token file to copy. It is only read.
        #[arg(long)]
        from: PathBuf,
        /// Store the token even if it replaces one that expires later, or is
        /// unusable (expiring, with no refresh token).
        #[arg(long)]
        force: bool,
    },
    /// Run the browser login on the registered callback port.
    Login {
        #[arg(long)]
        config: PathBuf,
    },
    /// Show the stored token's expiry and whether it has a refresh token.
    Status {
        #[arg(long)]
        config: PathBuf,
    },
}

impl AuthCommand {
    fn config(&self) -> &Path {
        match self {
            AuthCommand::Import { config, .. }
            | AuthCommand::Login { config }
            | AuthCommand::Status { config } => config,
        }
    }
}

/// The process. Returns its exit code.
pub fn run() -> i32 {
    let cli = Cli::parse();
    logging::init();
    let Command::Auth(command) = cli.command;

    let cwd = match std::env::current_dir() {
        Ok(cwd) => cwd,
        Err(err) => {
            tracing::error!(error = %err, "cannot read the working directory");
            return exit::FATAL;
        }
    };
    if let Err(refusal) = env_guard::check(std::env::vars_os(), &cwd) {
        tracing::error!("{refusal}");
        return exit::FATAL;
    }
    let config = match Config::load(command.config()) {
        Ok(config) => config,
        Err(err) => {
            tracing::error!("{err}");
            return exit::FATAL;
        }
    };
    // SAFETY: still single-threaded. Logging spawned no thread, and the
    // only runtime is built below, after this call.
    unsafe { env_guard::pin_process_region(config.access_region) };

    match command {
        AuthCommand::Import { from, force, .. } => import(&config, &from, force),
        AuthCommand::Login { .. } => login(&config),
        AuthCommand::Status { .. } => status(&config),
    }
}

/// Take the data-dir lock for an operator command, or say why not. `Err`
/// carries the exit code: `LOCKED` while another process holds it.
fn lock(config: &Config) -> Result<DataDirLock, i32> {
    DataDirLock::try_acquire(&config.data_dir).map_err(|err| match err {
        LockError::Busy(_) => {
            tracing::error!("{err}; nothing done, retry when it is free");
            exit::LOCKED
        }
        LockError::Io { .. } => {
            tracing::error!("{err}");
            exit::FATAL
        }
    })
}

fn writable_storage(config: &Config) -> Result<DataDirTokenStorage, i32> {
    DataDirTokenStorage::open(config.oauth_dir()).map_err(|err| {
        tracing::error!("{err}");
        exit::FATAL
    })
}

fn import(config: &Config, from: &Path, force: bool) -> i32 {
    let _lock = match lock(config) {
        Ok(lock) => lock,
        Err(code) => return code,
    };
    let storage = match writable_storage(config) {
        Ok(storage) => storage,
        Err(code) => return code,
    };
    match auth::import_token(
        from,
        &config.oauth_client_id,
        &storage,
        force,
        auth::unix_now(),
    ) {
        Ok(summary) => {
            println!("imported: {summary}");
            exit::OK
        }
        Err(err) => {
            tracing::error!("{err}");
            exit::FATAL
        }
    }
}

fn login(config: &Config) -> i32 {
    let _lock = match lock(config) {
        Ok(lock) => lock,
        Err(code) => return code,
    };
    let storage = match writable_storage(config) {
        Ok(storage) => storage,
        Err(code) => return code,
    };
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            tracing::error!(error = %err, "cannot start the async runtime");
            return exit::FATAL;
        }
    };
    let flow = runtime.block_on(auth::login(
        &config.oauth_client_id,
        storage.clone(),
        config.callback_port,
        |url| eprintln!("Open this URL in a browser to authorize the archive:\n{url}"),
    ));
    if let Err(err) = flow {
        // `err` is a fixed variant name, never the SDK's text: the callback's
        // `error` parameter is chosen by whoever reaches the port.
        tracing::error!("login failed: {err}");
        return exit::AUTH_UNAVAILABLE;
    }
    match storage.load_checked(&config.oauth_client_id) {
        Ok(Some(token)) => {
            println!("logged in: {}", TokenSummary::from(&token));
            exit::OK
        }
        Ok(None) => {
            tracing::error!("login finished but stored no token");
            exit::AUTH_UNAVAILABLE
        }
        Err(err) => {
            tracing::error!("{err}");
            exit::AUTH_UNAVAILABLE
        }
    }
}

/// Reads the stored token without the lock: saves replace the file by an
/// atomic rename, so a read sees the old or the new token, whole. A token
/// the SDK could not use without a browser login exits `AUTH_UNAVAILABLE`.
fn status(config: &Config) -> i32 {
    let storage = DataDirTokenStorage::read_only(config.oauth_dir());
    match storage.load_checked(&config.oauth_client_id) {
        Ok(Some(token)) => {
            let summary = TokenSummary::from(&token);
            println!("{summary}");
            if summary.usable_at(auth::unix_now()) {
                exit::OK
            } else {
                tracing::error!(
                    "the token is expiring or expired and has no refresh token; run `auth login`"
                );
                exit::AUTH_UNAVAILABLE
            }
        }
        Ok(None) => {
            tracing::error!("no token stored; run `auth import` or `auth login`");
            exit::AUTH_UNAVAILABLE
        }
        Err(err) => {
            tracing::error!("{err}");
            exit::AUTH_UNAVAILABLE
        }
    }
}
