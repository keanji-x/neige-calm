//! The read-only Neige plugin (#2575 S1b): serves the archive's
//! [`status`](crate::status) as the `status` tool, and keeps it live as the
//! `archive.status` table overlay on every Track that asked to `watch` it.
//!
//! It reads only what [`status::read`] reads. It holds no archive lock,
//! reads no token and calls no Longbridge host; the capture job stays the
//! systemd timer's. Its config is the data dir and two intervals, from the
//! kernel's handshake.

mod rpc;
pub mod view;

use std::collections::HashMap;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::status::{self, ArchiveStatus, Policy};
use crate::types::UnixMs;
use rpc::Rpc;

/// KV key prefix of the watching Tracks (`watch/<track_id>`).
const WATCH_PREFIX: &str = "watch/";

/// The plugin's config: every key comes from the kernel, which merged the
/// manifest's defaults in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Config {
    pub data_dir: PathBuf,
    pub poll: Duration,
    pub policy: Policy,
}

/// The poll interval's bounds, in seconds.
const POLL_SECONDS: (u64, u64) = (10, 3_600);
/// The staleness bound's bounds, in minutes. A start that holds the lock for
/// its whole attempt timeout writes `last_exit.json` up to about 55 minutes
/// after the one before, so less than an hour would cry wolf.
const STALE_AFTER_MINUTES: (u64, u64) = (60, 2_880);

/// Reads the config from the `initialize` request's
/// `_meta["dev.neige/config"].values`. A missing or invalid key is an error,
/// never a default: the kernel delivers the manifest defaults already.
pub fn config_from_initialize(init: &Value) -> Result<Config, String> {
    let values = init
        .pointer("/params/_meta/dev.neige~1config/values")
        .and_then(Value::as_object)
        .ok_or("the kernel delivered no configuration (`dev.neige/config`)")?;
    let data_dir = values
        .get("data_dir")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .ok_or("`data_dir` must be an absolute path")?;
    let bounded = |key: &str, (lo, hi): (u64, u64)| {
        values
            .get(key)
            .and_then(Value::as_u64)
            .filter(|n| (lo..=hi).contains(n))
            .ok_or_else(|| format!("`{key}` must be an integer from {lo} to {hi}"))
    };
    let poll = bounded("poll_seconds", POLL_SECONDS)?;
    let stale = bounded("stale_after_minutes", STALE_AFTER_MINUTES)?;
    Ok(Config {
        data_dir,
        poll: Duration::from_secs(poll),
        policy: Policy {
            stale_after_ms: i64::try_from(stale * 60_000).expect("bounded above"),
        },
    })
}

/// The current config: `None` before `initialize`, `Err` when it was bad.
type Shared = Arc<Mutex<Option<Result<Config, String>>>>;

fn current(config: &Shared) -> Result<Config, String> {
    config
        .lock()
        .map_err(|_| "config lock poisoned".to_owned())?
        .clone()
        .unwrap_or_else(|| Err("not initialized".to_owned()))
}

/// The tables last pushed, by Track. One lock serializes every overlay
/// change: the poller's pass and a `watch` call, so an unwatched Track
/// never gets its table back from a pass that listed it just before.
static PUBLISHED: Mutex<Option<HashMap<String, Value>>> = Mutex::new(None);

fn now() -> UnixMs {
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after 1970")
        .as_millis();
    UnixMs(i64::try_from(ms).expect("milliseconds fit in i64"))
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime builds")
}

fn read_status(rt: &tokio::runtime::Runtime, cfg: &Config) -> ArchiveStatus {
    rt.block_on(status::read(&cfg.data_dir, now(), cfg.policy))
}

/// The Track a call came from: `params._meta["dev.neige/track"].id`, which
/// the kernel fills from the caller's identity. Never the arguments.
fn track_from_call(frame: &Value) -> Option<String> {
    frame
        .pointer("/params/_meta/dev.neige~1track/id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty())
        .map(str::to_owned)
}

fn watch_key(track_id: &str) -> String {
    format!("{WATCH_PREFIX}{track_id}")
}

fn watchers(rpc: &Rpc) -> Result<Vec<String>, String> {
    let result = rpc.call("neige.kv.list", json!({ "prefix": WATCH_PREFIX }))?;
    let entries = result
        .get("entries")
        .and_then(Value::as_array)
        .ok_or("neige.kv.list returned no `entries` array")?;
    Ok(entries
        .iter()
        .filter_map(|e| e.get("key").and_then(Value::as_str))
        .filter_map(|k| k.strip_prefix(WATCH_PREFIX))
        .filter(|t| !t.is_empty())
        .map(str::to_owned)
        .collect())
}

/// One poll pass: the status, pushed to each watching Track whose table
/// changed. Nothing is read when no Track watches.
fn publish(rpc: &Rpc, rt: &tokio::runtime::Runtime, cfg: &Config) {
    let Ok(mut published) = PUBLISHED.lock() else {
        return;
    };
    let published = published.get_or_insert_with(HashMap::new);
    let tracks = match watchers(rpc) {
        Ok(tracks) => tracks,
        Err(e) => {
            eprintln!("research-archive: listing watchers failed: {e}");
            return;
        }
    };
    published.retain(|t, _| tracks.contains(t));
    if tracks.is_empty() {
        return;
    }
    let table = view::table(&read_status(rt, cfg));
    for track in tracks {
        if published.get(&track) == Some(&table) {
            continue;
        }
        let pushed = rpc.call(
            "neige.overlay.set",
            json!({
                "entity_kind": "track",
                "entity_id": track,
                "kind": view::OVERLAY_KIND,
                "payload": table,
            }),
        );
        match pushed {
            Ok(_) => {
                published.insert(track, table.clone());
            }
            Err(e) => eprintln!("research-archive: pushing the table failed: {e}"),
        }
    }
}

fn text_result(text: String, structured: Value) -> Value {
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": structured,
    })
}

fn tool_error(text: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": text.into() }], "isError": true })
}

fn tools_call_reply(
    rpc: &Rpc,
    rt: &tokio::runtime::Runtime,
    config: &Shared,
    wake: &mpsc::Sender<()>,
    frame: &Value,
) -> Value {
    let name = frame
        .pointer("/params/name")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let args = frame
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let cfg = match current(config) {
        Ok(cfg) => cfg,
        Err(e) => return tool_error(format!("research-archive is not configured: {e}")),
    };
    match name {
        "status" => {
            let status = read_status(rt, &cfg);
            let text = format!(
                "{} · checked {}",
                view::headline(&status),
                status::time(status.checked_at)
            );
            text_result(
                text,
                serde_json::to_value(&status).expect("a status serializes"),
            )
        }
        "watch" => {
            let Some(on) = args.get("on").and_then(Value::as_bool) else {
                return tool_error("`on` must be true or false");
            };
            let Some(track) = track_from_call(frame) else {
                return tool_error("watch acts on the calling Track, and this call has none");
            };
            match set_watch(rpc, &track, on) {
                Ok(()) => {
                    let _ = wake.send(());
                    let text = if on {
                        format!(
                            "This Track now shows the live `{}` table; name it in the Report \
                             with a table block whose source is \
                             neige://plugin/research-archive/{}.",
                            view::OVERLAY_KIND,
                            view::OVERLAY_KIND
                        )
                    } else {
                        "This Track no longer shows the archive status.".to_owned()
                    };
                    text_result(text, json!({ "watching": on }))
                }
                Err(e) => tool_error(e),
            }
        }
        other => tool_error(format!("unknown tool `{other}`")),
    }
}

/// Starts or stops one Track's table, under the publish lock. Either way the
/// poller forgets what it pushed there, so a new watch gets a fresh push.
fn set_watch(rpc: &Rpc, track: &str, on: bool) -> Result<(), String> {
    let mut published = PUBLISHED
        .lock()
        .map_err(|_| "publish lock poisoned".to_owned())?;
    if let Some(published) = published.as_mut() {
        published.remove(track);
    }
    if on {
        rpc.call(
            "neige.kv.set",
            json!({ "key": watch_key(track), "value": true }),
        )?;
    } else {
        rpc.call("neige.kv.delete", json!({ "key": watch_key(track) }))?;
        rpc.call(
            "neige.overlay.delete",
            json!({
                "entity_kind": "track",
                "entity_id": track,
                "kind": view::OVERLAY_KIND,
            }),
        )?;
    }
    Ok(())
}

fn initialize_reply(init: &Value) -> Value {
    let mut result = json!({
        "protocolVersion": init
            .pointer("/params/protocolVersion")
            .cloned()
            .unwrap_or_else(|| json!("2025-11-25")),
        "serverInfo": { "name": "research-archive", "version": env!("CARGO_PKG_VERSION") },
        "capabilities": {
            "tools": {},
            "experimental": { "dev.neige/kernel-callbacks": { "version": 1 } }
        },
    });
    if let Some(echo) = init
        .pointer("/params/_meta/dev.neige~1auth/expected_echo")
        .and_then(Value::as_str)
    {
        result["_meta"] = json!({ "dev.neige/auth": { "echoed_token": echo } });
    }
    result
}

/// The plugin process: reads the kernel's frames from stdin until it closes.
pub fn main() {
    let rpc = Arc::new(Rpc::new());
    let config: Shared = Arc::new(Mutex::new(None));

    // One worker for tool calls: not the read loop, because a tool's
    // callbacks are answered on the stdin that loop reads.
    let (tool_calls, tool_queue) = mpsc::channel::<Value>();
    // A `watch` wakes the poller. The sender lives as long as the process,
    // so the poller's wait never sees a disconnect and spins.
    let (wake_tx, wake_rx) = mpsc::channel::<()>();
    let mut wake_rx = Some(wake_rx);
    {
        let rpc = Arc::clone(&rpc);
        let config = Arc::clone(&config);
        let wake_tx = wake_tx.clone();
        std::thread::spawn(move || {
            let rt = runtime();
            for frame in tool_queue {
                let Some(id) = frame.get("id").cloned() else {
                    continue;
                };
                let reply = tools_call_reply(&rpc, &rt, &config, &wake_tx, &frame);
                rpc.reply(id, reply);
            }
        });
    }

    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { return };
        if line.trim().is_empty() {
            continue;
        }
        let frame: Value = match serde_json::from_str(&line) {
            Ok(frame) => frame,
            Err(e) => {
                eprintln!("research-archive: bad json from the kernel: {e}");
                continue;
            }
        };
        // A frame with no `method` replies to one of our callbacks.
        let Some(method) = frame.get("method").and_then(Value::as_str) else {
            match frame.get("id").and_then(Value::as_u64) {
                Some(id) if rpc.complete(id, frame) => {}
                Some(id) => {
                    eprintln!("research-archive: reply to {id} arrived with nobody waiting")
                }
                None => eprintln!("research-archive: a frame with neither method nor id"),
            }
            continue;
        };
        let Some(id) = frame.get("id").cloned() else {
            // A notification, such as `notifications/initialized`.
            continue;
        };
        match method {
            "initialize" => {
                rpc.reply(id, initialize_reply(&frame));
                let parsed = config_from_initialize(&frame);
                match &parsed {
                    Ok(cfg) => eprintln!(
                        "research-archive: configured, data_dir {}, poll {}s",
                        cfg.data_dir.display(),
                        cfg.poll.as_secs()
                    ),
                    Err(e) => eprintln!("research-archive: bad configuration: {e}"),
                }
                if let Ok(mut config) = config.lock() {
                    *config = Some(parsed);
                }
                let Some(wake_rx) = wake_rx.take() else {
                    // A running poller picks the new config up now.
                    let _ = wake_tx.send(());
                    continue;
                };
                {
                    let rpc = Arc::clone(&rpc);
                    let config = Arc::clone(&config);
                    std::thread::spawn(move || {
                        let rt = runtime();
                        loop {
                            let poll = match current(&config) {
                                Ok(cfg) => {
                                    publish(&rpc, &rt, &cfg);
                                    cfg.poll
                                }
                                Err(_) => Duration::from_secs(POLL_SECONDS.1),
                            };
                            // Wake early on a `watch`; drain a burst into one pass.
                            let _ = wake_rx.recv_timeout(poll);
                            while wake_rx.try_recv().is_ok() {}
                        }
                    });
                }
            }
            "tools/call" => {
                if tool_calls.send(frame).is_err() {
                    rpc.error(id, -32603, "tool worker unavailable");
                }
            }
            "ping" => rpc.reply(id, json!({})),
            other => rpc.error(id, -32601, &format!("method not found: {other}")),
        }
    }
}
