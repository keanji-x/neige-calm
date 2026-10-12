//! In-process tests whose SDK traffic must leave the machine-local sandbox
//! only through a proxy the test owns. One binary, because they set
//! process-wide environment (see `hermetic`); its tests take one lock, so
//! they never overlap even when run as threads of one process.

mod dotenv;
#[path = "../common/dotenv_reach.rs"]
mod dotenv_reach;
mod hermetic;
mod login;
mod refresh;
