//! A fake clock for the runner, and a fake server that stamps each request
//! with the fake time it arrived.

use std::sync::{Arc, Mutex};

use research_archive::clock::Clock;
use research_archive::types::UnixMs;

use crate::fake::{FakeServer, Reply, Seen};

/// A clock that moves only when told to, or when someone sleeps on it.
#[derive(Debug)]
pub struct FakeClock(Mutex<i64>);

impl FakeClock {
    pub fn at(t: UnixMs) -> Arc<FakeClock> {
        Arc::new(FakeClock(Mutex::new(t.0)))
    }

    pub fn set(&self, t: UnixMs) {
        *self.0.lock().unwrap() = t.0;
    }

    pub fn advance(&self, ms: i64) {
        *self.0.lock().unwrap() += ms;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> UnixMs {
        UnixMs(*self.0.lock().unwrap())
    }

    /// Moves the clock to `at` at once. The capture only sleeps on its
    /// clock while nothing is in flight (`clock::wait_until`).
    async fn sleep_until(&self, at: UnixMs) {
        {
            let mut now = self.0.lock().unwrap();
            *now = (*now).max(at.0);
        }
        tokio::task::yield_now().await;
    }
}

/// A request the timed server saw, with the fake time it arrived.
#[derive(Clone, Debug)]
pub struct Arrival {
    pub query: Option<String>,
    pub at: UnixMs,
    pub path: String,
    pub symbol: String,
}

/// A fake server that stamps each request with `clock` on arrival.
pub struct TimedServer {
    pub server: FakeServer,
    pub arrivals: Arc<Mutex<Vec<Arrival>>>,
}

impl TimedServer {
    pub async fn start(
        clock: Arc<FakeClock>,
        respond: impl Fn(&Seen, &FakeClock) -> Reply + Send + Sync + 'static,
    ) -> TimedServer {
        let arrivals = Arc::new(Mutex::new(Vec::new()));
        let log = arrivals.clone();
        let server = FakeServer::start(move |seen| {
            log.lock().unwrap().push(Arrival {
                query: seen.query.clone(),
                at: clock.now(),
                path: seen.path.clone(),
                symbol: symbol_of(seen),
            });
            respond(seen, &clock)
        })
        .await;
        TimedServer { server, arrivals }
    }

    pub fn arrivals(&self) -> Vec<Arrival> {
        self.arrivals.lock().unwrap().clone()
    }
}

/// The `symbol` query value of a request.
pub fn symbol_of(seen: &Seen) -> String {
    seen.query
        .as_deref()
        .unwrap_or("")
        .split('&')
        .find_map(|pair| pair.strip_prefix("symbol="))
        .unwrap_or("")
        .to_owned()
}
