//! The short state/commit barrier excludes all provider work. Existing REFRESH_LOCK
//! serializes history writers; setters never wait on that whole network pass.
use super::*;

pub(super) static STATE: Mutex<()> = Mutex::new(());
pub(super) const TOTAL_HISTORY_PREFIX: &str = "total_history/";

#[derive(PartialEq)]
struct Snapshot {
    holdings: Vec<Holding>,
    holdings_valid: bool,
    cash: Result<cash::Book, String>,
}

impl Snapshot {
    fn load(rpc: &Rpc, track: &str) -> Result<Self, String> {
        let result = rpc.call("neige.kv.get", json!({"key":holdings_key(track)}))?;
        let raw = result
            .get("value")
            .ok_or("holdings read returned no value field")?;
        let holdings_valid = raw.is_null()
            || raw
                .as_array()
                .is_some_and(|rows| rows.iter().all(|row| Holding::from_json(row).is_some()));
        Ok(Self {
            holdings: holdings_from_value(Some(raw), track),
            holdings_valid,
            cash: cash::load(rpc, track),
        })
    }
}

fn read_history(rpc: &Rpc, key: &str) -> Result<Vec<Value>, String> {
    let reply = rpc.call("neige.kv.get", json!({"key":key}))?;
    let value = reply
        .get("value")
        .ok_or("history read returned no value field")?;
    if value.is_null() {
        return Ok(Vec::new());
    }
    let points = value
        .as_array()
        .ok_or("history must be an array; refusing to overwrite it")?;
    if points.iter().any(|point| {
        point.get("at").and_then(Value::as_str).is_none()
            || !point
                .get("total")
                .and_then(Value::as_f64)
                .is_some_and(|v| v.is_finite() && v >= 0.0)
    }) {
        return Err("history contains an invalid observation; refusing to overwrite it".into());
    }
    Ok(points.clone())
}

fn publish_history(
    rpc: &Rpc,
    track: &str,
    key: &str,
    kind: &str,
    history: Result<Vec<Value>, String>,
    at: &str,
    total: f64,
    currency: &str,
) -> Result<(), String> {
    let mut points = history?;
    points.push(json!({"at":at,"total":round_to(total,2),"currency":currency}));
    if points.len() > MAX_HISTORY_POINTS {
        points.drain(0..points.len() - MAX_HISTORY_POINTS);
    }
    rpc.call("neige.kv.set", json!({"key":key,"value":points}))?;
    let mut payload = history_table(&points);
    if kind == "portfolio.total_history" {
        payload["as_of"] = json!(at);
        payload["caption"] = json!(format!(
            "Recorded securities and cash; value changes include balance changes, not investment returns. {}",
            payload["caption"].as_str().unwrap_or("")
        ));
    }
    if !push_overlay(rpc, track, kind, payload) {
        return Err(format!("{kind} could not be published"));
    }
    Ok(())
}

/// Caller retains STATE, including on error; a late unavailable projection
/// must not overwrite the result of a newer acknowledged setter.
fn unavailable(rpc: &Rpc, track: &str, at: &str, why: &str) -> Refreshed {
    let caption =
        format!("Portfolio state unavailable — {why}; no current combined total or weights");
    let allocation = portfolio_value::table(
        &[("label", "Asset"), ("value", "Value")],
        vec![
            json!({"id":"total","kind":"total","label":"Total","value":Value::Null,"currency":Value::Null}),
        ],
        at,
        &caption,
    );
    let positions = portfolio_value::table(
        &[
            ("asset", "Asset"),
            ("value", "Value"),
            ("weight", "Weight (%)"),
        ],
        Vec::new(),
        at,
        &caption,
    );
    let cash = portfolio_value::table(
        &[("currency", "Currency"), ("amount", "Current balance")],
        vec![
            json!({"currency":Value::Null,"amount":Value::Null,"value":Value::Null,"value_currency":Value::Null,"weight":Value::Null}),
        ],
        at,
        &caption,
    );
    for (kind, payload) in [
        ("portfolio.allocation", allocation),
        ("portfolio.positions", positions),
        ("portfolio.cash", cash),
    ] {
        push_overlay(rpc, track, kind, payload);
    }
    Refreshed::Partially(caption)
}

pub(super) fn refresh(rpc: &Rpc, cfg: &Config, track: &str, cache: &mut PassCache) -> Refreshed {
    let _refresh = match REFRESH_LOCK.lock() {
        Ok(lock) => lock,
        Err(_) => return Refreshed::Partially("refresh lock unavailable".into()),
    };
    let at = now_rfc3339();
    let snapshot = {
        let _state = match STATE.lock() {
            Ok(lock) => lock,
            Err(_) => return Refreshed::Partially("portfolio state lock unavailable".into()),
        };
        match Snapshot::load(rpc, track) {
            Ok(snapshot) => snapshot,
            Err(error) => return unavailable(rpc, track, &at, &error),
        }
    };
    // Provider requests and history reads cannot block a cash/holding setter.
    let priced = price_holdings(cfg, &snapshot.holdings, cache);
    let combined =
        portfolio_value::value(cfg, &priced, &snapshot.cash, snapshot.holdings_valid, cache);
    let old_total = priced
        .total
        .stated()
        .map(|(value, unit)| (value, unit.to_string()));
    let old_eligible = !snapshot.holdings.is_empty() && priced.complete && old_total.is_some();
    let new_eligible = combined.recorded && combined.total.is_some();
    let old_history = old_eligible.then(|| read_history(rpc, &history_key(track)));
    let new_key = format!("{TOTAL_HISTORY_PREFIX}{track}");
    let new_history = new_eligible.then(|| read_history(rpc, &new_key));

    let _state = match STATE.lock() {
        Ok(lock) => lock,
        Err(_) => return Refreshed::Partially("portfolio state lock unavailable".into()),
    };
    let current = match Snapshot::load(rpc, track) {
        Ok(snapshot) => snapshot,
        Err(error) => return unavailable(rpc, track, &at, &error),
    };
    // Canonical value equality deliberately permits A→B→A. This is sampled
    // current state, not an operation ledger; absent and explicit zero differ.
    if current != snapshot {
        return Refreshed::Stale;
    }

    let mut failures = Vec::<String>::new();
    let old_complete = priced.complete;
    let old_reason = priced.total.no_total_reason();
    let old_published = push_overlay(
        rpc,
        track,
        "portfolio.holdings",
        holdings_table(priced, &at),
    );
    if !old_published {
        failures.push("security holdings could not be published".into());
    }
    let mut new_published = true;
    for (kind, payload) in combined.projections(&at) {
        if !push_overlay(rpc, track, kind, payload) {
            new_published = false;
            failures.push(format!("{kind} could not be published"));
        }
    }
    if let (true, Some(history), Some((total, unit))) = (old_published, old_history, old_total) {
        if let Err(error) = publish_history(
            rpc,
            track,
            &history_key(track),
            "portfolio.history",
            history,
            &at,
            total,
            &unit,
        ) {
            failures.push(format!("security history: {error}"));
        }
    }
    if let (true, Some(history), Some((total, unit))) =
        (new_published, new_history, &combined.total)
    {
        if let Err(error) = publish_history(
            rpc,
            track,
            &new_key,
            "portfolio.total_history",
            history,
            &at,
            *total,
            unit,
        ) {
            failures.push(format!("combined history: {error}"));
        }
    }
    if !old_complete {
        failures.push("some securities could not be priced or converted".into());
    }
    if !snapshot.holdings.is_empty()
        && let Some(reason) = old_reason
    {
        failures.push(reason);
    }
    if combined.recorded && combined.total.is_none() {
        failures.push(combined.caption);
    }
    if !failures.is_empty() {
        Refreshed::Partially(failures.join("; "))
    } else if !combined.recorded {
        Refreshed::NothingHeld
    } else {
        Refreshed::Fully
    }
}
