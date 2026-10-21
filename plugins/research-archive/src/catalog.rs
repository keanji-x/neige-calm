//! The compiled endpoint catalog (#2575, "Longbridge endpoint catalog").
//!
//! Each entry fixes everything that shapes a request except the key: method,
//! path, ordered query and headers. Paths and query names come from
//! `rust/src/fundamental/context.rs` and `rust/src/market/context.rs` at
//! `longbridge/openapi@v5.2.0`. The archive freezes the catalog in its
//! `endpoint` table and `Store::open` refuses a file whose rows differ, so a
//! change to any field of an existing entry gets a new `endpoint_id`.

use std::collections::BTreeSet;

/// Every request carries this language. Pinning it makes the localized
/// strings in a body part of the endpoint's definition.
const HEADERS: &[(&str, &str)] = &[("Accept-Language", "en")];

/// The query parameter that carries the key on every catalog endpoint.
const KEY_PARAM: &str = "symbol";

/// What a key names.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum KeyKind {
    /// An index symbol in dotted form, such as `.SPX.US`.
    Index,
    /// A security symbol such as `AAPL.US`.
    Security,
    /// A market-wide request, never a security symbol.
    Market,
}

impl KeyKind {
    /// The `endpoint.key_kind` value.
    pub fn as_str(self) -> &'static str {
        match self {
            KeyKind::Index => "index",
            KeyKind::Security => "security",
            KeyKind::Market => "market",
        }
    }
}

/// The HTTP method of an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    /// Paged HTTP GET with the frozen calendar_v1 body codec.
    Calendar,
    /// Quote WebSocket business command; path contains its fixed identifier.
    Quote,
}

impl Method {
    /// The `endpoint.method` value.
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Calendar => "GET_CALENDAR_V1",
            Method::Quote => "QUOTE",
        }
    }
}

/// A data center that alone can serve an entry. The SDK refuses such a
/// request locally when the token's prefix names another one. No current
/// entry is restricted (the `us_*` endpoints were dropped after probe P4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DcRegion {
    Us,
}

impl DcRegion {
    /// The `endpoint.dc_restrict` value.
    pub fn as_str(self) -> &'static str {
        match self {
            DcRegion::Us => "us",
        }
    }
}

/// One query pair's value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryValue {
    /// The run key (a symbol) is sent here.
    Key,
    /// A literal value.
    Fixed(&'static str),
    /// ISO date derived from the run trading day, never invocation wall time.
    AsOfDays(i64),
}

/// A catalog endpoint id, such as `consensus`. Ids only come from a
/// [`Catalog`], so every one names a row of the archive's `endpoint` table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EndpointId(&'static str);

impl EndpointId {
    pub fn as_str(self) -> &'static str {
        self.0
    }
}

impl std::fmt::Display for EndpointId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// One catalog entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Endpoint {
    pub id: EndpointId,
    pub method: Method,
    pub path: &'static str,
    /// The query pairs, in the order they are sent.
    pub query: &'static [(&'static str, QueryValue)],
    pub headers: &'static [(&'static str, &'static str)],
    pub dc_restrict: Option<DcRegion>,
    pub key_kind: KeyKind,
}

impl Endpoint {
    /// A GET entry with the pinned headers and no data-center restriction.
    pub const fn new(
        id: &'static str,
        path: &'static str,
        query: &'static [(&'static str, QueryValue)],
        key_kind: KeyKind,
    ) -> Endpoint {
        Endpoint {
            id: EndpointId(id),
            method: Method::Get,
            path,
            query,
            headers: HEADERS,
            dc_restrict: None,
            key_kind,
        }
    }

    /// The canonical `endpoint.query_json`: a compact JSON array of
    /// `[name, value]` pairs in send order, where the value `null` stands
    /// for the run key. Example: `[["symbol",null],["indicator","pe"]]`.
    pub fn query_json(&self) -> String {
        let pairs: Vec<serde_json::Value> = self
            .query
            .iter()
            .map(|(name, value)| {
                let value = match value {
                    QueryValue::Key => serde_json::Value::Null,
                    QueryValue::Fixed(v) => serde_json::json!(v),
                    QueryValue::AsOfDays(days) => serde_json::json!({"as_of_days": days}),
                };
                serde_json::json!([name, value])
            })
            .collect();
        serde_json::to_string(&pairs).expect("catalog values serialize")
    }

    /// Resolve the complete request once, at planning time.
    pub fn resolve(&self, key: &str, as_of: crate::types::AsOf) -> Vec<(String, String)> {
        self.query
            .iter()
            .map(|(name, value)| {
                let value = match value {
                    QueryValue::Key => key.to_owned(),
                    QueryValue::Fixed(v) => (*v).to_owned(),
                    QueryValue::AsOfDays(days) => as_of
                        .date()
                        .checked_add_signed(chrono::Duration::days(*days))
                        .expect("validated run date plus bounded catalog offset")
                        .to_string(),
                };
                ((*name).to_owned(), value)
            })
            .collect()
    }

    /// The canonical `endpoint.headers_json`: a compact JSON array of
    /// `[name, value]` pairs in send order.
    pub fn headers_json(&self) -> String {
        let pairs: Vec<(&str, Option<&str>)> =
            self.headers.iter().map(|(n, v)| (*n, Some(*v))).collect();
        canonical_json(&pairs)
    }
}

/// The one serialization of `query_json` and `headers_json`: serde_json's
/// compact form (no whitespace, minimal string escapes, UTF-8 kept as is) of
/// an array of `[name, value]` arrays in order, `None` written as `null`.
fn canonical_json(pairs: &[(&str, Option<&str>)]) -> String {
    serde_json::to_string(pairs).expect("string pairs always serialize")
}

const KEY_ONLY: &[(&str, QueryValue)] = &[(KEY_PARAM, QueryValue::Key)];

const COMPILED: &[Endpoint] = &[
    Endpoint::new(
        "consensus",
        "/v1/quote/financial-consensus-detail",
        KEY_ONLY,
        KeyKind::Security,
    ),
    Endpoint::new(
        "forecast_eps",
        "/v1/quote/forecast-eps",
        KEY_ONLY,
        KeyKind::Security,
    ),
    Endpoint::new(
        "rating_latest",
        "/v1/quote/institution-rating-latest",
        KEY_ONLY,
        KeyKind::Security,
    ),
    Endpoint::new(
        "ratings",
        "/v1/quote/institution-ratings",
        KEY_ONLY,
        KeyKind::Security,
    ),
    Endpoint::new(
        "rating_detail",
        "/v1/quote/institution-ratings/detail",
        KEY_ONLY,
        KeyKind::Security,
    ),
    Endpoint::new(
        "valuation",
        "/v1/quote/valuation",
        &[
            (KEY_PARAM, QueryValue::Key),
            ("indicator", QueryValue::Fixed("pe")),
            ("range", QueryValue::Fixed("1")),
        ],
        KeyKind::Security,
    ),
    // One call returns every member: without paging the endpoint returns 20
    // of `total`, and pages of 100 are inconsistent (probe P5b).
    Endpoint::new(
        "index_constituents",
        "/v1/quote/index-constituents",
        &[
            (KEY_PARAM, QueryValue::Key),
            ("offset", QueryValue::Fixed("0")),
            ("limit", QueryValue::Fixed("1000")),
        ],
        KeyKind::Index,
    ),
    Endpoint::new(
        "consensus_annual",
        "/v1/quote/financial-consensus-detail",
        &[
            (KEY_PARAM, QueryValue::Key),
            ("period_type", QueryValue::Fixed("af")),
        ],
        KeyKind::Security,
    ),
    Endpoint::new(
        "earnings_snapshot",
        "/v1/quote/financials/earnings-snapshot",
        KEY_ONLY,
        KeyKind::Security,
    ),
    Endpoint::new(
        "financial_reports",
        "/v1/quote/financial-reports",
        &[
            (KEY_PARAM, QueryValue::Key),
            ("kind", QueryValue::Fixed("ALL")),
            ("report", QueryValue::Fixed("qf")),
        ],
        KeyKind::Security,
    ),
    Endpoint {
        method: Method::Calendar,
        ..Endpoint::new(
            "finance_calendar",
            "/v1/quote/finance_calendar",
            &[
                ("date", QueryValue::AsOfDays(-7)),
                ("date_end", QueryValue::AsOfDays(30)),
                ("types[]", QueryValue::Fixed("report")),
                ("markets[]", QueryValue::Fixed("US")),
                ("count", QueryValue::Fixed("100")),
                ("next", QueryValue::Fixed("later")),
            ],
            KeyKind::Market,
        )
    },
    Endpoint {
        method: Method::Quote,
        ..Endpoint::new(
            "daily_candlesticks",
            "quote:27",
            &[
                (KEY_PARAM, QueryValue::Key),
                ("start", QueryValue::AsOfDays(-14)),
                ("end", QueryValue::AsOfDays(0)),
                ("period", QueryValue::Fixed("day")),
                ("adjust", QueryValue::Fixed("none")),
                ("session", QueryValue::Fixed("regular")),
            ],
            KeyKind::Security,
        )
    },
];

/// A set of endpoints with unique ids, in a fixed order, with exactly one
/// index endpoint: the membership endpoint.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalog {
    endpoints: Vec<Endpoint>,
    membership: EndpointId,
}

/// A list of endpoints that is not a catalog.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    #[error("catalog endpoint id {0} appears more than once")]
    DuplicateId(&'static str),
    #[error("a catalog needs exactly one index endpoint, this one has {0}")]
    IndexEndpoints(usize),
}

impl Catalog {
    /// The catalog this binary was built with.
    pub fn compiled() -> Catalog {
        Catalog::new(COMPILED.to_vec()).expect("the compiled catalog is valid")
    }

    /// Frozen released v2 catalog used only by the explicit legacy upgrade.
    pub fn released_v2() -> Catalog {
        Catalog::new(COMPILED[..8].to_vec()).expect("released catalog")
    }

    pub fn new(endpoints: Vec<Endpoint>) -> Result<Catalog, CatalogError> {
        let mut seen = BTreeSet::new();
        for e in &endpoints {
            if !seen.insert(e.id) {
                return Err(CatalogError::DuplicateId(e.id.0));
            }
        }
        let index: Vec<EndpointId> = endpoints
            .iter()
            .filter(|e| e.key_kind == KeyKind::Index)
            .map(|e| e.id)
            .collect();
        match index[..] {
            [membership] => Ok(Catalog {
                endpoints,
                membership,
            }),
            _ => Err(CatalogError::IndexEndpoints(index.len())),
        }
    }

    pub fn endpoints(&self) -> &[Endpoint] {
        &self.endpoints
    }

    /// The entry with this id.
    pub fn get(&self, id: &str) -> Option<&Endpoint> {
        self.endpoints.iter().find(|e| e.id.0 == id)
    }

    /// The one index endpoint, whose `ok` body is an index's membership
    /// (`index_constituents`). Its bodies must pass
    /// [`crate::membership::constituents_members`].
    pub fn membership(&self) -> EndpointId {
        self.membership
    }

    /// The entries that apply to keys of `kind`, in catalog order.
    pub fn for_kind(&self, kind: KeyKind) -> impl Iterator<Item = &Endpoint> {
        self.endpoints.iter().filter(move |e| e.key_kind == kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The issue's table, row by row, in the stored (canonical) form.
    #[test]
    fn annual_consensus_endpoint_uses_verified_period_type() {
        let catalog = Catalog::compiled();
        let annual = catalog
            .get("consensus_annual")
            .expect("annual endpoint is captured");
        assert_eq!(annual.path, "/v1/quote/financial-consensus-detail");
        assert_eq!(
            annual.query_json(),
            r#"[["symbol",null],["period_type","af"]]"#
        );
        assert_eq!(
            catalog.get("consensus").unwrap().query_json(),
            r#"[["symbol",null]]"#
        );
    }

    #[test]
    fn compiled_catalog_matches_the_issue_table_golden() {
        let rows: Vec<_> = Catalog::compiled()
            .endpoints()
            .iter()
            .map(|e| {
                (
                    e.id.as_str(),
                    e.method.as_str(),
                    e.path,
                    e.query_json(),
                    e.headers_json(),
                    e.dc_restrict.map(DcRegion::as_str),
                    e.key_kind.as_str(),
                )
            })
            .collect();
        let key = r#"[["symbol",null]]"#.to_owned();
        let en = r#"[["Accept-Language","en"]]"#.to_owned();
        let security = |id, path| (id, "GET", path, key.clone(), en.clone(), None, "security");
        assert_eq!(
            rows,
            [
                security("consensus", "/v1/quote/financial-consensus-detail"),
                security("forecast_eps", "/v1/quote/forecast-eps"),
                security("rating_latest", "/v1/quote/institution-rating-latest"),
                security("ratings", "/v1/quote/institution-ratings"),
                security("rating_detail", "/v1/quote/institution-ratings/detail"),
                (
                    "valuation",
                    "GET",
                    "/v1/quote/valuation",
                    r#"[["symbol",null],["indicator","pe"],["range","1"]]"#.to_owned(),
                    en.clone(),
                    None,
                    "security",
                ),
                (
                    "index_constituents",
                    "GET",
                    "/v1/quote/index-constituents",
                    r#"[["symbol",null],["offset","0"],["limit","1000"]]"#.to_owned(),
                    en.clone(),
                    None,
                    "index",
                ),
                (
                    "consensus_annual",
                    "GET",
                    "/v1/quote/financial-consensus-detail",
                    r#"[["symbol",null],["period_type","af"]]"#.to_owned(),
                    en.clone(),
                    None,
                    "security",
                ),
                security("earnings_snapshot", "/v1/quote/financials/earnings-snapshot"),
                ("financial_reports", "GET", "/v1/quote/financial-reports",
                    r#"[["symbol",null],["kind","ALL"],["report","qf"]]"#.to_owned(),en.clone(),None,"security"),
                ("finance_calendar", "GET_CALENDAR_V1", "/v1/quote/finance_calendar",
                    r#"[["date",{"as_of_days":-7}],["date_end",{"as_of_days":30}],["types[]","report"],["markets[]","US"],["count","100"],["next","later"]]"#.to_owned(),en.clone(),None,"market"),
                ("daily_candlesticks", "QUOTE", "quote:27",
                    r#"[["symbol",null],["start",{"as_of_days":-14}],["end",{"as_of_days":0}],["period","day"],["adjust","none"],["session","regular"]]"#.to_owned(),en.clone(),None,"security"),
            ]
        );
    }

    #[test]
    fn canonical_json_escapes_and_orders_deterministically() {
        const Q: &[(&str, QueryValue)] = &[
            ("z", QueryValue::Fixed("a\"b\\c")),
            (KEY_PARAM, QueryValue::Key),
            ("a", QueryValue::Fixed("é")),
        ];
        let e = Endpoint::new("x", "/x", Q, KeyKind::Security);
        assert_eq!(
            e.query_json(),
            r#"[["z","a\"b\\c"],["symbol",null],["a","é"]]"#
        );
        assert_eq!(e.query_json(), e.query_json());
    }

    #[test]
    fn ids_are_unique_and_lookups_agree() {
        let catalog = Catalog::compiled();
        for e in catalog.endpoints() {
            assert_eq!(catalog.get(e.id.as_str()), Some(e));
        }
        assert_eq!(catalog.for_kind(KeyKind::Security).count(), 10);
        assert_eq!(catalog.for_kind(KeyKind::Market).count(), 1);
        assert_eq!(catalog.for_kind(KeyKind::Index).count(), 1);
        assert!(catalog.get("missing").is_none());
        assert_eq!(catalog.membership().as_str(), "index_constituents");
        let dup = vec![COMPILED[0], COMPILED[0], COMPILED[6]];
        assert!(matches!(
            Catalog::new(dup),
            Err(CatalogError::DuplicateId("consensus"))
        ));
        let none = COMPILED[..6].to_vec();
        assert!(matches!(
            Catalog::new(none),
            Err(CatalogError::IndexEndpoints(0))
        ));
        let two = vec![
            COMPILED[6],
            Endpoint {
                id: EndpointId("index_v2"),
                ..COMPILED[6]
            },
        ];
        assert!(matches!(
            Catalog::new(two),
            Err(CatalogError::IndexEndpoints(2))
        ));
    }
}
