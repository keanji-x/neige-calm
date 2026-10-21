//! The `read_at` tool: archived security bodies as they stood at a past
//! moment, each narrowed by a JSON pointer.
//!
//! Every read is [`ReadOnlyStore::read_at`], the Store's one point-in-time
//! read, so the tool answers what the read contract says: the latest `ok`
//! row with `fetched_at ≤ at`. The readable endpoints are the compiled
//! catalog's security endpoints; the manifest's enum is pinned to them by a
//! test. The answer is never truncated: over [`MAX_ANSWER_BYTES`] the call
//! fails with each read's size.

use std::path::Path;

use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::catalog::{Catalog, EndpointId, KeyKind};
use crate::store::{ReadOnlyStore, Store};
use crate::types::{Key, UnixMs};

/// The most reads one call takes.
pub const MAX_READS: usize = 12;

/// The cap on the answer: the serialized JSON the tool's text carries (its
/// `structuredContent` is the same object). Codex shows the model only
/// `structuredContent` and middle-truncates a tool output over its budget of
/// 10,000 tokens × 1.2 × 4 bytes ≈ 48,000 bytes; 40 KiB keeps a margin under
/// it. Claude Code's MCP cap (25,000 tokens) is higher, so Codex binds.
pub const MAX_ANSWER_BYTES: usize = 40 * 1024;
const _: () = assert!(
    MAX_ANSWER_BYTES < 48_000,
    "under Codex's tool-output budget"
);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    at: String,
    reads: Vec<ReadArgs>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum ReadArgs {
    Security(SecurityRead),
    Market(MarketRead),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SecurityRead {
    symbol: String,
    endpoint: String,
    pointer: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MarketRead {
    market: String,
    endpoint: String,
    pointer: String,
}

/// One checked read.
#[derive(Clone, Debug)]
struct Read {
    kind: KeyKind,
    key: Key,
    endpoint: EndpointId,
    /// An RFC 6901 pointer: empty, or starting with `/`.
    pointer: String,
}

/// A checked call.
#[derive(Clone, Debug)]
pub struct Request {
    at: UnixMs,
    reads: Vec<Read>,
}

/// The endpoints a read may name: the catalog's security endpoints.
pub fn readable(catalog: &Catalog) -> impl Iterator<Item = EndpointId> + '_ {
    catalog
        .endpoints()
        .iter()
        .filter(|e| e.key_kind != KeyKind::Index)
        .map(|e| e.id)
}

/// Checks the call's arguments. Every refusal names what to fix.
pub fn parse(args: &Value) -> Result<Request, String> {
    if let Some(reads) = args.get("reads").and_then(Value::as_array) {
        for (i, read) in reads.iter().enumerate() {
            if let Some(fields) = read.as_object() {
                for field in fields.keys() {
                    if !["symbol", "market", "endpoint", "pointer"].contains(&field.as_str()) {
                        return Err(format!("reads[{i}]: unknown field `{field}`"));
                    }
                }
            }
            for field in ["endpoint", "pointer"] {
                if read.get(field).is_none() {
                    return Err(format!("reads[{i}]: missing field `{field}`"));
                }
            }
            if read.get("symbol").is_none() && read.get("market").is_none() {
                return Err(format!("reads[{i}]: missing field `symbol` or `market`"));
            }
        }
    }
    let args = Args::deserialize(args).map_err(|e| format!("bad arguments: {e}"))?;
    let at = DateTime::parse_from_rfc3339(&args.at).map_err(|e| {
        format!(
            "`at` must be an RFC 3339 time with an offset, such as \
             2026-10-13T16:00:00-04:00 ({e}): {:?}",
            args.at
        )
    })?;
    if !(1..=MAX_READS).contains(&args.reads.len()) {
        return Err(format!(
            "`reads` takes 1 to {MAX_READS} reads, not {}",
            args.reads.len()
        ));
    }
    let catalog = Catalog::compiled();
    let reads = args
        .reads
        .into_iter()
        .enumerate()
        .map(|(i, r)| {
            let (key, kind, endpoint, pointer) = match r {
                ReadArgs::Security(r) => (
                    Key::new(r.symbol).map_err(|e| format!("reads[{i}].symbol: {e}"))?,
                    KeyKind::Security,
                    r.endpoint,
                    r.pointer,
                ),
                ReadArgs::Market(r) => {
                    if r.market != "US" {
                        return Err(format!("reads[{i}].market must be US"));
                    }
                    (
                        Key::new("market:US").expect("fixed market key"),
                        KeyKind::Market,
                        r.endpoint,
                        r.pointer,
                    )
                }
            };
            let endpoint_name = endpoint;
            let endpoint = readable(&catalog)
                .find(|id| id.as_str() == endpoint_name)
                .ok_or_else(|| {
                    let ids: Vec<&str> = readable(&catalog).map(EndpointId::as_str).collect();
                    format!(
                        "reads[{i}].endpoint {:?} is not readable; one of: {}",
                        endpoint_name,
                        ids.join(", ")
                    )
                })?;
            if catalog
                .get(endpoint.as_str())
                .expect("readable endpoint")
                .key_kind
                != kind
            {
                return Err(format!(
                    "reads[{i}] target kind does not match endpoint {endpoint}"
                ));
            }
            if !is_pointer(&pointer) {
                return Err(format!(
                    "reads[{i}].pointer {:?} is not an RFC 6901 JSON pointer: use \"\" for \
                     the whole body or start with /, and escape ~ as ~0 and / as ~1",
                    pointer
                ));
            }
            Ok(Read {
                kind,
                key,
                endpoint,
                pointer,
            })
        })
        .collect::<Result<_, String>>()?;
    Ok(Request {
        at: UnixMs(at.timestamp_millis()),
        reads,
    })
}

/// Whether `p` is an RFC 6901 pointer: empty, or `/`-led, with every `~`
/// followed by `0` or `1`.
fn is_pointer(p: &str) -> bool {
    let escapes_ok = p
        .split('~')
        .skip(1)
        .all(|rest| rest.starts_with('0') || rest.starts_with('1'));
    (p.is_empty() || p.starts_with('/')) && escapes_ok
}

/// What one read asked for, echoed in its answer.
#[derive(Clone, Debug, Serialize)]
#[serde(untagged)]
enum Target {
    Security {
        symbol: String,
        endpoint: &'static str,
        pointer: String,
    },
    Market {
        market: &'static str,
        endpoint: &'static str,
        pointer: String,
    },
}

/// What one read found.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "result", rename_all = "snake_case")]
enum Found {
    /// The body's value at the pointer, from the latest `ok` row by `at`.
    Value {
        as_of: String,
        fetched_at: String,
        value: Value,
    },
    /// No run ever planned this symbol × endpoint.
    NotArchived,
    /// Planned, but no `ok` row was fetched by `at`.
    NoValue,
    /// The row's body has nothing at the pointer.
    PointerMissing { as_of: String, fetched_at: String },
}

#[derive(Clone, Debug, Serialize)]
struct Answer {
    #[serde(flatten)]
    target: Target,
    #[serde(flatten)]
    found: Found,
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ`.
fn rfc3339(t: UnixMs) -> Result<String, String> {
    DateTime::<Utc>::from_timestamp_millis(t.0)
        .map(|t| t.to_rfc3339_opts(SecondsFormat::Millis, true))
        .ok_or_else(|| format!("{} ms is not a representable time", t.0))
}

/// Answers a checked call from the archive in `data_dir`: the JSON object
/// the tool returns, or the reason the call fails.
pub async fn answer(data_dir: &Path, request: &Request) -> Result<Value, String> {
    let store = ReadOnlyStore::open(&Store::path(data_dir))
        .await
        .map_err(|e| format!("the archive cannot be read: {e}"))?;
    let mut answers = Vec::with_capacity(request.reads.len());
    let mut failed = None;
    for (i, read) in request.reads.iter().enumerate() {
        match read_one(&store, request.at, read).await {
            Ok(a) => answers.push(a),
            Err(e) => {
                failed = Some(format!("reads[{i}] ({} {}): {e}", read.key, read.endpoint));
                break;
            }
        }
    }
    store.close().await;
    if let Some(e) = failed {
        return Err(e);
    }
    let payload = json!({ "at": rfc3339(request.at)?, "reads": answers });
    let size = payload.to_string().len();
    if size > MAX_ANSWER_BYTES {
        let sizes: Vec<String> = answers
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let (target, endpoint, pointer) = match &a.target {
                    Target::Security {
                        symbol,
                        endpoint,
                        pointer,
                    } => (symbol.as_str(), *endpoint, pointer.as_str()),
                    Target::Market {
                        market,
                        endpoint,
                        pointer,
                    } => (*market, *endpoint, pointer.as_str()),
                };
                format!(
                    "reads[{i}] {target} {endpoint} {pointer:?}: {} bytes",
                    serde_json::to_string(a).map_or(0, |s| s.len())
                )
            })
            .collect();
        return Err(format!(
            "the answer is {size} bytes, over the {MAX_ANSWER_BYTES}-byte cap; nothing was \
             truncated. Narrow the large reads with a pointer, or split the call. {}",
            sizes.join("; ")
        ));
    }
    Ok(payload)
}

async fn read_one(store: &ReadOnlyStore, at: UnixMs, read: &Read) -> Result<Answer, String> {
    let target = match read.kind {
        KeyKind::Market => Target::Market {
            market: "US",
            endpoint: read.endpoint.as_str(),
            pointer: read.pointer.clone(),
        },
        KeyKind::Security => Target::Security {
            symbol: read.key.to_string(),
            endpoint: read.endpoint.as_str(),
            pointer: read.pointer.clone(),
        },
        KeyKind::Index => unreachable!("index endpoints are not readable"),
    };
    let row = store
        .read_at(&read.key, read.endpoint, at)
        .await
        .map_err(|e| e.to_string())?;
    let found = match row {
        Some(row) => {
            let (as_of, fetched_at) = (row.as_of.to_string(), rfc3339(row.fetched_at)?);
            // The capture stores the envelope's `data` JSON as is, so a
            // security body is always JSON; anything else is a broken file.
            let body: Value = serde_json::from_slice(&row.body)
                .map_err(|e| format!("the body fetched at {fetched_at} is not JSON ({e})"))?;
            match body.pointer(&read.pointer) {
                Some(value) => Found::Value {
                    as_of,
                    fetched_at,
                    value: value.clone(),
                },
                None => Found::PointerMissing { as_of, fetched_at },
            }
        }
        None if store
            .ever_planned(&read.key, read.endpoint)
            .await
            .map_err(|e| e.to_string())? =>
        {
            Found::NoValue
        }
        None => Found::NotArchived,
    };
    Ok(Answer { target, found })
}
