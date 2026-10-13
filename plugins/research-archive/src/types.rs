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

    /// Whether `t` is inside: `start ≤ t < end`.
    pub fn contains(self, t: UnixMs) -> bool {
        self.start <= t && t < self.end
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

/// Where a failure without a usable body came from (`observation.error_class`).
/// Every failure row has an error class with a non-empty `error_code` whose
/// meaning depends on it. The stored set has one more value, which no caller
/// can state: `incomplete_membership`, derived by the Store from a
/// constituents body (see [`Response::Body`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorClass {
    /// An OpenAPI envelope with a non-zero code. Code: that code, in decimal
    /// (for example `429002`).
    Envelope,
    /// A non-success HTTP status outside an envelope. Code: the status, in
    /// decimal.
    HttpStatus,
    /// No HTTP response: a timeout or a connection error. Code: the SDK
    /// error variant in snake_case (for example `request_timeout`).
    Transport,
    /// A response that is not a usable envelope (no `data`, or not JSON).
    /// Code: the SDK error variant.
    UnexpectedResponse,
    /// The SDK had no usable credentials. Code: the SDK error variant (for
    /// example `invalid_access_token`).
    Auth,
    /// The SDK refused locally: the token's data center cannot serve the
    /// endpoint. Code: the SDK error variant. Always `region_restricted`.
    DcRegionRestricted,
    /// The SDK could not build the request. Code: the SDK error variant.
    Client,
}

impl ErrorClass {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorClass::Envelope => "envelope",
            ErrorClass::HttpStatus => "http_status",
            ErrorClass::Transport => "transport",
            ErrorClass::UnexpectedResponse => "unexpected_response",
            ErrorClass::Auth => "auth",
            ErrorClass::DcRegionRestricted => "dc_region_restricted",
            ErrorClass::Client => "client",
        }
    }
}

/// A failure's `(error_class, error_code)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ErrorDetail {
    pub class: ErrorClass,
    pub code: String,
}

/// What one attempt produced, as the caller saw it. The caller never
/// classifies a body: the Store does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    /// A response body arrived: the envelope's exact `data` bytes (empty
    /// bytes included). On a security endpoint it is stored as `ok`. On the
    /// membership endpoint the Store applies
    /// [`constituents_members`](crate::membership::constituents_members): a
    /// complete body is `ok`, an incomplete one is `permanent` with
    /// `error_class = incomplete_membership` and the rule that failed as
    /// `error_code`, and its bytes are kept either way.
    Body(Vec<u8>),
    /// No usable body: a transport, HTTP, envelope, auth or client failure.
    /// No bytes are stored for it.
    Failed {
        class: FailureClass,
        error: ErrorDetail,
    },
}

/// What `record_observation` stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Recorded {
    /// The attempt number within `(run, key, endpoint)`, from 1.
    pub attempt: u32,
    pub status: Status,
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

/// One planned index's frozen membership (`run_membership`), as the Store
/// derived it at the freeze.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Membership {
    /// Members from this run's latest `ok` constituents observation.
    Observed { members: Vec<Key> },
    /// Members from `source`'s latest `ok` constituents observation: the run
    /// with the latest earlier `as_of` that has one. That observation is the
    /// original, never a carry.
    Carried { source: RunId, members: Vec<Key> },
    /// No earlier or current run has an `ok` constituents observation.
    None,
}

impl Membership {
    /// The `run_membership.mode` value.
    pub fn mode(&self) -> &'static str {
        self.kind().as_str()
    }

    pub fn kind(&self) -> MembershipMode {
        match self {
            Membership::Observed { .. } => MembershipMode::Observed,
            Membership::Carried { .. } => MembershipMode::Carried,
            Membership::None => MembershipMode::None,
        }
    }

    pub fn members(&self) -> &[Key] {
        match self {
            Membership::Observed { members } | Membership::Carried { members, .. } => members,
            Membership::None => &[],
        }
    }
}

/// `run_membership.mode`: how a frozen index membership was derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MembershipMode {
    Observed,
    Carried,
    None,
}

impl MembershipMode {
    pub fn as_str(self) -> &'static str {
        match self {
            MembershipMode::Observed => "observed",
            MembershipMode::Carried => "carried",
            MembershipMode::None => "none",
        }
    }

    pub fn parse(s: &str) -> Result<MembershipMode, InvalidValue> {
        match s {
            "observed" => Ok(MembershipMode::Observed),
            "carried" => Ok(MembershipMode::Carried),
            "none" => Ok(MembershipMode::None),
            other => Err(invalid("mode", other)),
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
        for s in ["observed", "carried", "none"] {
            assert_eq!(MembershipMode::parse(s).unwrap().as_str(), s);
        }
        assert!(MembershipMode::parse("Observed").is_err());
        assert!(Key::new("").is_err());
        assert!(Key::new("A B").is_err());
        assert!(Window::new(UnixMs(2), UnixMs(2)).is_err());
    }
}
