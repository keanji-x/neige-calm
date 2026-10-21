//! The typed TOML config (#2575, "Typed config"). Every field is required:
//! a value missing from the file is an error, never a default. See
//! `research-archive.example.toml` for a commented sample.
//!
//! The config is the only input that steers the job. The environment is
//! refused ([`crate::env_guard`]), and the URLs and the region are passed to
//! the SDK explicitly.

use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::NaiveTime;
use serde::Deserialize;

use crate::auth::{CallbackPort, ClientId};
use crate::env_guard::AccessRegion;
use crate::source::Rps;
use crate::types::Key;

/// The file as written. Validated into [`Config`].
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawConfig {
    data_dir: PathBuf,
    oauth_client_id: String,
    access_region: AccessRegion,
    http_url: String,
    quote_ws_url: String,
    callback_port: u16,
    research: Vec<String>,
    indexes: Vec<String>,
    rps: u32,
    attempt_cap: u32,
    attempt_timeout_minutes: u32,
    carry_deadline_minutes: u32,
    window_start: String,
    notify_command: Vec<String>,
}

/// A validated config.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    /// Absolute. Holds `archive.sqlite3`, `archive.lock` and `oauth/`.
    pub data_dir: PathBuf,
    pub oauth_client_id: ClientId,
    pub access_region: AccessRegion,
    /// The OpenAPI base URL: `https://host[:port]`, no path.
    pub http_url: String,
    /// The quote WebSocket URL: `wss://…`.
    pub quote_ws_url: String,
    /// The redirect port registered for the OAuth client; only `auth login`
    /// binds it.
    pub callback_port: CallbackPort,
    /// Security keys (`AAPL.US`), unique.
    pub research: Vec<Key>,
    /// Index symbols in dotted form (`.SPX.US`), unique. The membership
    /// request asks for `limit=1000`, so an index with more than 1,000
    /// members never yields a complete membership.
    pub indexes: Vec<Key>,
    pub rps: Rps,
    /// Attempts per key × endpoint for `transient` and `permanent` failures
    /// within a window. Throttles and auth pauses do not count.
    pub attempt_cap: u32,
    /// The longest one capture invocation runs.
    pub attempt_timeout: Duration,
    /// How long after `window_start` the membership waits before carrying.
    pub carry_deadline: Duration,
    /// The window's start on each trading day, New York local time.
    pub window_start: NaiveTime,
    /// The alert command's argv, run without a shell. The capture runner
    /// (S1-2b) runs it with an allowlisted environment, never the job's own.
    pub notify_command: Vec<String>,
}

/// Why a config was refused. Messages name the field; the file holds no
/// credential.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read config {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("config {path}: {message}")]
    Parse { path: PathBuf, message: String },
    #[error("config field `{field}`: {reason}")]
    Invalid { field: &'static str, reason: String },
}

fn invalid(field: &'static str, reason: impl Into<String>) -> ConfigError {
    ConfigError::Invalid {
        field,
        reason: reason.into(),
    }
}

impl Config {
    /// Read and validate the config at `path`, a regular file of bounded
    /// size ([`crate::files::read_regular`]).
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let read_err = |source| ConfigError::Read {
            path: path.to_owned(),
            source,
        };
        let bytes = crate::files::read_regular(path).map_err(read_err)?;
        let text = String::from_utf8(bytes).map_err(|_| {
            read_err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "not UTF-8",
            ))
        })?;
        Config::parse(&text).map_err(|err| match err {
            ConfigError::Parse { message, .. } => ConfigError::Parse {
                path: path.to_owned(),
                message,
            },
            other => other,
        })
    }

    /// Validate config text.
    pub fn parse(text: &str) -> Result<Config, ConfigError> {
        let raw: RawConfig = toml::from_str(text).map_err(|e| ConfigError::Parse {
            path: PathBuf::new(),
            message: e.message().to_owned(),
        })?;
        raw.validate()
    }

    /// `<data_dir>/archive.sqlite3`.
    pub fn archive_path(&self) -> PathBuf {
        crate::store::Store::path(&self.data_dir)
    }

    /// `<data_dir>/oauth`, the token storage directory.
    pub fn oauth_dir(&self) -> PathBuf {
        self.data_dir.join("oauth")
    }
}

impl RawConfig {
    fn validate(self) -> Result<Config, ConfigError> {
        if !self.data_dir.is_absolute() {
            return Err(invalid("data_dir", "must be an absolute path"));
        }
        let oauth_client_id = ClientId::parse(&self.oauth_client_id)
            .map_err(|e| invalid("oauth_client_id", e.to_string()))?;
        let callback_port = CallbackPort::new(self.callback_port)
            .ok_or_else(|| invalid("callback_port", "must be the registered port, not 0"))?;
        let research = keys("research", &self.research, KeyForm::Security)?;
        let indexes = keys("indexes", &self.indexes, KeyForm::Index)?;
        if research.is_empty() && indexes.is_empty() {
            return Err(invalid("research", "research and indexes are both empty"));
        }
        let rps = Rps::new(self.rps)
            .ok_or_else(|| invalid("rps", format!("must be 1..={}", Rps::MAX)))?;
        let attempt_cap = in_range("attempt_cap", self.attempt_cap, 1..=100)?;
        let attempt_timeout = minutes(in_range(
            "attempt_timeout_minutes",
            self.attempt_timeout_minutes,
            1..=720,
        )?);
        let carry_deadline = minutes(in_range(
            "carry_deadline_minutes",
            self.carry_deadline_minutes,
            1..=600,
        )?);
        let window_start = NaiveTime::parse_from_str(&self.window_start, "%H:%M")
            .ok()
            .filter(|t| t.format("%H:%M").to_string() == self.window_start)
            .ok_or_else(|| invalid("window_start", "must be HH:MM, New York local time"))?;
        // The regular session closes at 16:00 New York; the window opens after.
        if window_start < NaiveTime::from_hms_opt(16, 0, 0).expect("a valid time") {
            return Err(invalid("window_start", "must be at or after 16:00"));
        }
        let shortest = shortest_window_minutes(window_start);
        if u32::try_from(shortest).is_ok_and(|shortest| self.carry_deadline_minutes >= shortest) {
            return Err(invalid(
                "carry_deadline_minutes",
                format!(
                    "must end before the shortest window: at most {} for window_start {}, \
                     which can close {shortest} minutes later (at 09:30 New York the next \
                     day, one hour sooner across the spring DST change)",
                    shortest - 1,
                    self.window_start
                ),
            ));
        }
        match self.notify_command.first() {
            Some(program) if Path::new(program).is_absolute() => {}
            _ => {
                return Err(invalid(
                    "notify_command",
                    "must be an argv list whose first entry is an absolute path",
                ));
            }
        }
        Ok(Config {
            data_dir: self.data_dir,
            oauth_client_id,
            access_region: self.access_region,
            http_url: base_url("http_url", &self.http_url, "https")?,
            quote_ws_url: ws_url("quote_ws_url", &self.quote_ws_url)?,
            callback_port,
            research,
            indexes,
            rps,
            attempt_cap,
            attempt_timeout,
            carry_deadline,
            window_start,
            notify_command: self.notify_command,
        })
    }
}

/// The shortest window that opens at `window_start` (New York): to 09:30
/// New York the next day, one hour shorter when the clocks spring forward
/// in between. Longer windows (weekends, holidays) only start the same way.
pub fn shortest_window_minutes(window_start: NaiveTime) -> i64 {
    use chrono::Timelike;
    let start = i64::from(window_start.hour() * 60 + window_start.minute());
    24 * 60 + (9 * 60 + 30) - start - 60
}

fn in_range(
    field: &'static str,
    value: u32,
    range: std::ops::RangeInclusive<u32>,
) -> Result<u32, ConfigError> {
    if range.contains(&value) {
        Ok(value)
    } else {
        Err(invalid(
            field,
            format!("must be {}..={}", range.start(), range.end()),
        ))
    }
}

fn minutes(value: u32) -> Duration {
    Duration::from_secs(u64::from(value) * 60)
}

#[derive(Clone, Copy)]
enum KeyForm {
    /// `CODE.MARKET`, as `AAPL.US` or `BRK.B.US`.
    Security,
    /// A dot, then the security form: `.SPX.US`.
    Index,
}

/// Whether `s` is `CODE.MARKET`: `CODE` is upper-case letters, digits, `.`
/// and `-`, starting with a letter or digit; `MARKET` is two upper-case
/// letters (`US`, `HK`, `SH`, `SZ`, `SG`).
fn is_security_symbol(s: &str) -> bool {
    let Some((code, market)) = s.rsplit_once('.') else {
        return false;
    };
    market.len() == 2
        && market.bytes().all(|b| b.is_ascii_uppercase())
        && code
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
        && code
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'.' || b == b'-')
        && !code.ends_with('.')
}

fn keys(field: &'static str, values: &[String], form: KeyForm) -> Result<Vec<Key>, ConfigError> {
    let mut seen = BTreeSet::new();
    let mut out = Vec::with_capacity(values.len());
    for value in values {
        let ok = match form {
            KeyForm::Security => is_security_symbol(value),
            KeyForm::Index => value.strip_prefix('.').is_some_and(is_security_symbol),
        };
        if !ok {
            let expected = match form {
                KeyForm::Security => "a security symbol such as AAPL.US",
                KeyForm::Index => "an index symbol in dotted form such as .SPX.US",
            };
            return Err(invalid(field, format!("{value:?} is not {expected}")));
        }
        if !seen.insert(value.as_str()) {
            return Err(invalid(field, format!("{value:?} appears twice")));
        }
        out.push(Key::new(value.as_str()).map_err(|e| invalid(field, e.to_string()))?);
    }
    Ok(out)
}

/// `scheme://host[:port]` with nothing after it: the SDK appends the request
/// path to the string as is (`request.rs:353`). Plain `http` is accepted only
/// for a loopback IP literal, a local test server.
fn base_url(field: &'static str, value: &str, scheme: &str) -> Result<String, ConfigError> {
    let loopback = url::Url::parse(value).is_ok_and(|u| {
        u.scheme() == "http"
            && match u.host() {
                Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                _ => false,
            }
    });
    let url = parse_url(field, value, if loopback { "http" } else { scheme })?;
    if url.path() != "/" || value.ends_with('/') {
        return Err(invalid(field, "must have no path and no trailing slash"));
    }
    Ok(value.to_owned())
}

fn ws_url(field: &'static str, value: &str) -> Result<String, ConfigError> {
    let loopback = url::Url::parse(value).is_ok_and(|u| {
        u.scheme() == "ws"
            && match u.host() {
                Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
                Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
                _ => false,
            }
    });
    parse_url(field, value, if loopback { "ws" } else { "wss" }).map(|_| value.to_owned())
}

fn parse_url(field: &'static str, value: &str, scheme: &str) -> Result<url::Url, ConfigError> {
    let url = url::Url::parse(value).map_err(|e| invalid(field, format!("not a URL: {e}")))?;
    if url.scheme() != scheme {
        return Err(invalid(field, format!("must use {scheme}://")));
    }
    if url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            field,
            "must name a host, with no credentials, query or fragment",
        ));
    }
    Ok(url)
}

#[cfg(test)]
mod tests;
