//! The process environment for in-process SDK tests.
//!
//! - **One black-hole proxy per process.** Every proxy variable points at a
//!   local listener that records each request line and never answers. Both
//!   reqwest versions read these variables by default (reqwest 0.12 for the
//!   SDK client, `async_impl/client.rs:419-421`; reqwest 0.11 for oauth2's
//!   token requests, `async_impl/client.rs:255`, `proxy.rs:897-918`) and
//!   cache them for the process, so the proxy is created once.
//! - **Pinned, not removed.** dotenv never overrides a variable that is
//!   already set (`dotenv-0.15.0/src/iter.rs:22-31`), so `NO_PROXY`,
//!   `no_proxy` and `LONGBRIDGE_ENV` are pinned to empty values, and
//!   `LONGBRIDGE_REGION` to `global`, which also skips the SDK's geo probe.
//! - **No `.env` in reach** of the working directory, checked by [`enter`].
//!
//! Tokens are fabricated. A refresh or code exchange sends
//! `CONNECT openapi.longbridge.com:443` to the listener and goes no further.

use std::{
    io::{BufRead, BufReader},
    net::TcpListener,
    sync::{Arc, Mutex, MutexGuard, OnceLock},
};

/// The request line of every OAuth token request through the proxy.
pub const OAUTH_CONNECT: &str = "CONNECT openapi.longbridge.com:443 HTTP/1.1";

pub struct Proxy {
    pub url: String,
    seen: Arc<Mutex<Vec<String>>>,
}

impl Proxy {
    /// The request lines received so far.
    pub fn seen(&self) -> Vec<String> {
        self.seen.lock().unwrap().clone()
    }

    fn clear(&self) {
        self.seen.lock().unwrap().clear();
    }

    /// The number of OAuth token requests; panics on any other traffic.
    pub fn oauth_requests(&self) -> usize {
        let seen = self.seen();
        assert!(
            seen.iter().all(|line| line == OAUTH_CONNECT),
            "unexpected proxy traffic: {seen:?}"
        );
        seen.len()
    }
}

fn start_proxy() -> Proxy {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    std::thread::spawn(move || {
        let mut held = Vec::new();
        for stream in listener.incoming() {
            let Ok(stream) = stream else { break };
            let mut line = String::new();
            let _ = BufReader::new(stream.try_clone().unwrap()).read_line(&mut line);
            log.lock().unwrap().push(line.trim_end().to_owned());
            held.push(stream);
        }
    });
    Proxy { url, seen }
}

static SERIAL: Mutex<()> = Mutex::new(());
static PROXY: OnceLock<Proxy> = OnceLock::new();

/// The process's proxy, with the environment pinned to it. Idempotent.
pub fn pinned() -> &'static Proxy {
    PROXY.get_or_init(|| {
        let proxy = start_proxy();
        // SAFETY: the only other thread is the proxy, which never touches
        // the environment; callers hold `SERIAL`, and no runtime exists yet
        // in this process.
        unsafe {
            for var in [
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "ALL_PROXY",
                "http_proxy",
                "https_proxy",
                "all_proxy",
            ] {
                std::env::set_var(var, &proxy.url);
            }
            for var in ["NO_PROXY", "no_proxy", "LONGBRIDGE_ENV"] {
                std::env::set_var(var, "");
            }
            std::env::set_var("LONGBRIDGE_REGION", "global");
        }
        proxy
    })
}

/// Serialize with the other tests of this binary and pin the environment.
/// `check_dotenv` refuses to run with a `.env` in reach of the working
/// directory; only the test that proves the pins hold against one skips it.
pub fn enter(check_dotenv: bool) -> (MutexGuard<'static, ()>, &'static Proxy) {
    let serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    if check_dotenv {
        crate::dotenv_reach::assert_no_dotenv_reach();
    }
    let proxy = pinned();
    proxy.clear();
    (serial, proxy)
}
