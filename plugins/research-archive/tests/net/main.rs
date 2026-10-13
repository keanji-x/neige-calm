//! Every test that builds an SDK `OAuth` or a `LongbridgeSource` in process,
//! the capture runner's included.
//! One binary, because it sets process-wide environment ([`hermetic`]):
//! every test runs under `hermetic::enter`, which serializes them, pins the
//! region, the proxies and `NO_PROXY`, and refuses a `.env` in reach. The
//! [`scan`] test keeps such constructions out of every other test.

#[path = "../common/archive.rs"]
#[allow(dead_code)]
mod archive;
mod auth;
mod calendar;
#[path = "../common/cli_env.rs"]
#[allow(dead_code)]
mod cli_env;
mod crossing;
mod deadline;
mod dotenv;
#[path = "../common/dotenv_reach.rs"]
mod dotenv_reach;
mod exit_state;
mod fake;
mod fake_clock;
mod hermetic;
mod interrupt;
mod login;
mod logs;
mod membership;
mod refresh;
mod runner;
mod scan;
mod schedule;
mod secrets;
mod source;
#[path = "../suite/support.rs"]
#[allow(dead_code)]
mod support;
