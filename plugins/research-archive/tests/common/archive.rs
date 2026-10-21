//! Shared fixtures for the capture tests, in process (`net::runner`) and as
//! a subprocess (the suite's `capture_*`): a config and data dir under
//! `TMPDIR`, a seeded calendar cache and a fabricated token. Nothing here
//! builds an SDK client.

use std::path::PathBuf;

use chrono::{DateTime, NaiveDate};
use research_archive::Store;
use research_archive::auth::{DataDirTokenStorage, Secret, TokenFile};
use research_archive::calendar::TradingCalendar;
use research_archive::config::Config;
use research_archive::types::UnixMs;

#[path = "quote.rs"]
mod quote;

pub const CLIENT: &str = "test-client";

/// Unix ms of an RFC 3339 instant.
pub fn t(s: &str) -> UnixMs {
    UnixMs(DateTime::parse_from_rfc3339(s).unwrap().timestamp_millis())
}

pub fn day(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

/// A local port nothing listens on.
pub fn closed_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// The config fields a test varies.
#[derive(Clone)]
pub struct Knobs {
    pub research: Vec<&'static str>,
    pub indexes: Vec<&'static str>,
    pub attempt_cap: u32,
    pub attempt_timeout_minutes: u32,
    pub carry_deadline_minutes: u32,
    pub rps: u32,
    pub window_start: &'static str,
    pub notify_command: Vec<String>,
}

impl Default for Knobs {
    fn default() -> Self {
        Knobs {
            research: vec!["AAPL.US"],
            indexes: vec![],
            attempt_cap: 3,
            attempt_timeout_minutes: 25,
            carry_deadline_minutes: 60,
            rps: 20,
            window_start: "17:30",
            notify_command: vec!["/bin/true".into()],
        }
    }
}

/// A private root with a data dir and a config pointing at `http_url`.
pub struct Archive {
    pub root: tempfile::TempDir,
    pub data_dir: PathBuf,
    pub config_path: PathBuf,
    pub http_url: String,
    quote_url: String,
}

fn quoted(items: &[impl AsRef<str>]) -> String {
    items
        .iter()
        .map(|s| format!("{:?}", s.as_ref()))
        .collect::<Vec<_>>()
        .join(", ")
}

impl Archive {
    pub fn new(http_url: &str, knobs: &Knobs) -> Archive {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("data");
        let archive = Archive {
            config_path: root.path().join("archive.toml"),
            data_dir,
            root,
            http_url: http_url.to_owned(),
            quote_url: quote::start(),
        };
        archive.write_config(knobs);
        archive
    }

    /// (Re)write the config file.
    pub fn write_config(&self, knobs: &Knobs) {
        let text = format!(
            r#"data_dir = "{}"
oauth_client_id = "{CLIENT}"
access_region = "cn"
http_url = "{}"
quote_ws_url = "{}"
callback_port = {}
research = [{}]
indexes = [{}]
rps = {}
attempt_cap = {}
attempt_timeout_minutes = {}
carry_deadline_minutes = {}
window_start = "{}"
notify_command = [{}]
"#,
            self.data_dir.display(),
            self.http_url,
            self.quote_url,
            closed_port(),
            quoted(&knobs.research),
            quoted(&knobs.indexes),
            knobs.rps,
            knobs.attempt_cap,
            knobs.attempt_timeout_minutes,
            knobs.carry_deadline_minutes,
            knobs.window_start,
            quoted(&knobs.notify_command),
        );
        std::fs::write(&self.config_path, text).unwrap();
    }

    pub fn config(&self) -> Config {
        Config::load(&self.config_path).unwrap()
    }

    /// Store a fabricated token directly, as `auth import` would.
    pub fn token(&self, access: &str, refresh: Option<&str>, expires_at: u64) {
        DataDirTokenStorage::open(self.data_dir.join("oauth"))
            .unwrap()
            .save_checked(&TokenFile {
                client_id: CLIENT.into(),
                access_token: Secret::new(access),
                refresh_token: refresh.map(Secret::new),
                expires_at,
            })
            .unwrap();
    }

    /// Seed the calendar cache, fetched at `fetched_at`.
    pub fn calendar(&self, fetched_at: UnixMs, from: &str, to: &str, days: &[&str]) {
        TradingCalendar {
            fetched_at: fetched_at.0,
            from: day(from),
            to: day(to),
            trading_days: days.iter().map(|d| day(d)).collect(),
            half_days: vec![],
        }
        .save(&self.data_dir)
        .unwrap();
    }

    pub async fn store(&self) -> Store {
        Store::open(&self.data_dir.join("archive.sqlite3"))
            .await
            .unwrap()
    }

    pub fn last_exit(&self) -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(self.data_dir.join("last_exit.json")).unwrap())
            .unwrap()
    }
}

/// A complete constituents body.
pub fn members(symbols: &[&str]) -> String {
    let stocks: Vec<String> = symbols
        .iter()
        .map(|s| format!(r#"{{"symbol":"{s}"}}"#))
        .collect();
    format!(
        r#"{{"total":{},"stocks":[{}]}}"#,
        symbols.len(),
        stocks.join(",")
    )
}

/// Expected security pairs derive from the declared catalog, not a frozen count.
pub fn security_count() -> usize {
    research_archive::Catalog::compiled()
        .for_kind(research_archive::KeyKind::Security)
        .count()
}
