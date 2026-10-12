//! Every test that builds an SDK `OAuth` or a `LongbridgeSource` in process.
//! One binary, because it sets process-wide environment ([`hermetic`]):
//! every test runs under `hermetic::enter`, which serializes them, pins the
//! region, the proxies and `NO_PROXY`, and refuses a `.env` in reach. The
//! [`scan`] test keeps such constructions out of every other test.

mod auth;
#[path = "../common/cli_env.rs"]
#[allow(dead_code)]
mod cli_env;
mod crossing;
mod dotenv;
#[path = "../common/dotenv_reach.rs"]
mod dotenv_reach;
mod fake;
mod hermetic;
mod login;
mod logs;
mod refresh;
mod scan;
mod secrets;
mod source;
#[path = "../suite/support.rs"]
#[allow(dead_code)]
mod support;
