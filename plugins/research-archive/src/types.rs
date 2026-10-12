//! The typed values the Store reads and writes. Each one maps to exactly one
//! stored representation, documented on its `as_str`/`sql` accessor and in
//! the README.

use std::fmt;

use chrono::NaiveDate;

/// A value that does not fit its type.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid {what}: {value:?}")]
pub struct InvalidValue {
    pub what: &'static str,
    pub value: String,
}

fn invalid(what: &'static str, value: impl Into<String>) -> InvalidValue {
    InvalidValue {
        what,
        value: value.into(),
    }
}

/// A wall-clock instant in integer milliseconds since the Unix epoch, UTC.
/// Every stored timestamp (`fetched_at`, `started_at`, `frozen_at`,
/// `finished_at`, `window_start`, `window_end`) uses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct UnixMs(pub i64);

/// The trading day a run is for, stored as an ISO date `YYYY-MM-DD`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AsOf(NaiveDate);

impl AsOf {
    /// A day with a four-digit year (0000–9999), the only years the stored
    /// form and the column's CHECK allow.
    pub fn new(date: NaiveDate) -> Result<AsOf, InvalidValue> {
        use chrono::Datelike;
        if (0..=9999).contains(&date.year()) {
            Ok(AsOf(date))
        } else {
            Err(invalid("as_of date", date.to_string()))
        }
    }

    pub fn date(self) -> NaiveDate {
        self.0
    }

    /// Parses the stored form. Only `YYYY-MM-DD` with a four-digit year is
    /// accepted, matching the column's CHECK.
    pub fn parse(s: &str) -> Result<AsOf, InvalidValue> {
        NaiveDate::parse_from_str(s, "%Y-%m-%d")
            .ok()
            .and_then(|date| AsOf::new(date).ok())
            .filter(|as_of| as_of.to_string() == s)
            .ok_or_else(|| invalid("as_of date", s))
    }
}

impl fmt::Display for AsOf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.format("%Y-%m-%d"))
    }
}

/// The capture window of a run: `start ≤ t < end`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    start: UnixMs,
    end: UnixMs,
}

impl Window {
    pub fn new(start: UnixMs, end: UnixMs) -> Result<Window, InvalidValue> {
        if start < end {
            Ok(Window { start, end })
        } else {
            Err(invalid("window", format!("{} >= {}", start.0, end.0)))
        }
    }

    pub fn start(self) -> UnixMs {
        self.start
    }

    pub fn end(self) -> UnixMs {
        self.end
    }
}

/// The upstream a run captures. Only Longbridge exists; Wisburg is deferred.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Longbridge,
}

impl Source {
    /// The `run.source` value.
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Longbridge => "longbridge",
        }
    }

    pub fn parse(s: &str) -> Result<Source, InvalidValue> {
        match s {
            "longbridge" => Ok(Source::Longbridge),
            other => Err(invalid("source", other)),
        }
    }
}

/// A run id: `run.run_id`, assigned by `plan_run` and `record_missed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RunId(pub i64);

impl fmt::Display for RunId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// A symbol the archive fetches for: a security (`AAPL.US`) or an index in
/// dotted form (`.SPX.US`). Non-empty, with no whitespace or control
/// characters.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Key(String);

impl Key {
    pub fn new(s: impl Into<String>) -> Result<Key, InvalidValue> {
        let s = s.into();
        if s.is_empty() || s.chars().any(|c| c.is_whitespace() || c.is_control()) {
            Err(invalid("key", s))
        } else {
            Ok(Key(s))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a key is in a run (`run_key.origin`). Each origin is a separate
/// membership fact; one key may carry several.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Origin {
    /// The research list, as configured when the run was planned.
    Config,
    /// A configured index symbol itself, fetched with the index endpoints.
    IndexSymbol,
    /// A member of the named index, written by the freeze.
    Index(Key),
}

impl Origin {
    /// The stored form: `config`, `index-symbol` or `index:<sym>`.
    pub fn to_sql(&self) -> String {
        match self {
            Origin::Config => "config".to_owned(),
            Origin::IndexSymbol => "index-symbol".to_owned(),
            Origin::Index(sym) => format!("index:{sym}"),
        }
    }

    pub fn parse(s: &str) -> Result<Origin, InvalidValue> {
        match s {
            "config" => Ok(Origin::Config),
            "index-symbol" => Ok(Origin::IndexSymbol),
            other => match other.strip_prefix("index:") {
                Some(sym) => Key::new(sym).map(Origin::Index),
                None => Err(invalid("origin", other)),
            },
        }
    }
}

/// A failure class: the attempt produced no observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FailureClass {
    Transient,
    Throttled,
    AuthUnavailable,
    RegionRestricted,
    Permanent,
}

/// `observation.status`. Only `Ok` is state-bearing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Ok,
    Failed(FailureClass),
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Failed(FailureClass::Transient) => "transient",
            Status::Failed(FailureClass::Throttled) => "throttled",
            Status::Failed(FailureClass::AuthUnavailable) => "auth_unavailable",
            Status::Failed(FailureClass::RegionRestricted) => "region_restricted",
            Status::Failed(FailureClass::Permanent) => "permanent",
        }
    }

    pub fn parse(s: &str) -> Result<Status, InvalidValue> {
        Ok(match s {
            "ok" => Status::Ok,
            "transient" => Status::Failed(FailureClass::Transient),
            "throttled" => Status::Failed(FailureClass::Throttled),
            "auth_unavailable" => Status::Failed(FailureClass::AuthUnavailable),
            "region_restricted" => Status::Failed(FailureClass::RegionRestricted),
            "permanent" => Status::Failed(FailureClass::Permanent),
            other => return Err(invalid("status", other)),
        })
    }
}

/// What one attempt produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The envelope's exact `data` bytes. Empty bytes are a valid value.
    Ok { body: Vec<u8> },
    /// No observation. `body` keeps bytes worth auditing (for example an
    /// `index_constituents` body that failed the membership check).
    Failed {
        class: FailureClass,
        body: Option<Vec<u8>>,
        error_class: Option<String>,
        error_code: Option<String>,
    },
}

impl Outcome {
    pub fn status(&self) -> Status {
        match self {
            Outcome::Ok { .. } => Status::Ok,
            Outcome::Failed { class, .. } => Status::Failed(*class),
        }
    }
}

/// How a run ends by itself (`end_run`). `missed` is only written by
/// `record_missed`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finish {
    Complete,
    Partial,
}

/// `run_end.outcome`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunOutcome {
    Complete,
    Partial,
    Missed,
}

impl RunOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            RunOutcome::Complete => "complete",
            RunOutcome::Partial => "partial",
            RunOutcome::Missed => "missed",
        }
    }

    pub fn parse(s: &str) -> Result<RunOutcome, InvalidValue> {
        match s {
            "complete" => Ok(RunOutcome::Complete),
            "partial" => Ok(RunOutcome::Partial),
            "missed" => Ok(RunOutcome::Missed),
            other => Err(invalid("outcome", other)),
        }
    }
}

impl From<Finish> for RunOutcome {
    fn from(f: Finish) -> RunOutcome {
        match f {
            Finish::Complete => RunOutcome::Complete,
            Finish::Partial => RunOutcome::Partial,
        }
    }
}

/// One planned index's frozen membership (`run_membership`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Membership {
    /// Members from this run's own `ok` constituents observation.
    Observed { members: Vec<Key> },
    /// Members carried from `source`, the run holding the original `ok`
    /// constituents observation (never an earlier carry).
    Carried { source: RunId, members: Vec<Key> },
    /// No `ok` membership exists anywhere yet.
    None,
}

impl Membership {
    /// The `run_membership.mode` value.
    pub fn mode(&self) -> &'static str {
        match self {
            Membership::Observed { .. } => "observed",
            Membership::Carried { .. } => "carried",
            Membership::None => "none",
        }
    }

    pub fn members(&self) -> &[Key] {
        match self {
            Membership::Observed { members } | Membership::Carried { members, .. } => members,
            Membership::None => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_forms_round_trip() {
        for s in ["2026-10-12", "1999-01-01"] {
            assert_eq!(AsOf::parse(s).unwrap().to_string(), s);
        }
        for s in [
            "2026-1-2",
            "2026-02-30",
            "26-10-12",
            " 2026-10-12",
            "+12026-10-12",
        ] {
            assert!(AsOf::parse(s).is_err(), "{s}");
        }
        for s in ["config", "index-symbol", "index:.SPX.US"] {
            assert_eq!(Origin::parse(s).unwrap().to_sql(), s);
        }
        for s in ["index:", "index", "Config", "index: x"] {
            assert!(Origin::parse(s).is_err(), "{s}");
        }
        for s in [
            "ok",
            "transient",
            "throttled",
            "auth_unavailable",
            "region_restricted",
            "permanent",
        ] {
            assert_eq!(Status::parse(s).unwrap().as_str(), s);
        }
        assert!(Status::parse("not_found").is_err());
        for s in ["complete", "partial", "missed"] {
            assert_eq!(RunOutcome::parse(s).unwrap().as_str(), s);
        }
        assert!(Key::new("").is_err());
        assert!(Key::new("A B").is_err());
        assert!(Window::new(UnixMs(2), UnixMs(2)).is_err());
    }
}
