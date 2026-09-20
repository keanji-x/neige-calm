//! `summary` on input and control receipts: a flat digest of facts the receipt already
//! carries. No new facts, no fence reads it; the digest is evidence, not a verdict.
use serde_json::{Value, json};

/// The digest of `receipt`. Every field is nullable; `control_id` is the readback's (explicit
/// null included), never the receipt's own lease.
pub fn receipt_summary(action: &str, receipt: &Value) -> Value {
    let action = receipt["outcome"].as_str().unwrap_or(action);
    let readback = match receipt["observation"]["status"].as_str() {
        Some("available") => "available",
        Some("unavailable") => "unavailable",
        _ => "none",
    };
    let state = match readback {
        "available" => &receipt["observation"]["state"],
        _ => &Value::Null,
    };
    let wait = &state["wait"];
    // `changed_since_previous_observation` is `previous.is_some_and(..)`, so a connection's
    // first observation says `false` although nothing was compared: the screen fact exists
    // only against a previous observation.
    let screen = match (
        &state["previous_observation_revision"],
        &state["changed_since_previous_observation"],
    ) {
        (Value::Null, _) => Value::Null,
        (_, Value::Bool(true)) => json!("changed"),
        (_, Value::Bool(false)) => json!("unchanged"),
        _ => Value::Null,
    };
    json!({"action":action,"readback":readback,
        "screen":screen,"wait":wait["outcome"],"settled":wait["settled"],
        "signal":wait["signal"]["event"],"repaint":wait["repaint"]["outcome"],
        "matched":wait["text"]["pattern"],
        "role":state["role"],"control_id":state["control_id"],"exited":state["exited"],
        "claim":receipt["claim"]["status"],"release":receipt["release"]["status"]})
}

/// The one-line text block saying the same in words. A null `screen` has no segment, and
/// `settled` qualifies the screen segment so it is not rendered either.
pub fn summary_line(terminal: &str, summary: &Value) -> String {
    let word = |value: &Value| match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let action = word(&summary["action"]);
    let mut parts = vec![match action.as_str() {
        "claim" | "release" => format!("terminal {terminal} {action}"),
        _ => format!("terminal {terminal} input {action}"),
    }];
    match summary["readback"].as_str() {
        Some("available") => {
            if !summary["screen"].is_null() {
                let settled = if summary["settled"] == true {
                    " settled"
                } else {
                    ""
                };
                parts.push(format!("screen {}{settled}", word(&summary["screen"])));
            }
            parts.push(format!("wait {}", word(&summary["wait"])));
            if !summary["signal"].is_null() {
                let repaint = match &summary["repaint"] {
                    Value::Null => String::new(),
                    outcome => format!(", repaint {}", word(outcome)),
                };
                parts.push(format!("signal {}{repaint}", word(&summary["signal"])));
            }
            if !summary["matched"].is_null() {
                parts.push(format!("matched {}", word(&summary["matched"])));
            }
            let exited = if summary["exited"] == true {
                " exited"
            } else {
                ""
            };
            parts.push(format!("role {}{exited}", word(&summary["role"])));
        }
        Some("unavailable") => parts.push("readback unavailable".into()),
        _ => parts.push("no readback".into()),
    }
    for fact in ["claim", "release"] {
        if !summary[fact].is_null() {
            parts.push(format!("{fact} {}", word(&summary[fact])));
        }
    }
    parts.push("details in structuredContent".into());
    parts.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A readback state; `Value::Null` for `changed` leaves the field out.
    fn state(wait: Value, changed: Value, role: &str, control_id: Value) -> Value {
        json!({"observation_id":"o-2","observation_revision":"9","role":role,"control_id":control_id,
            "exited":false,"text":["$ "],"wait":wait,"changed_since_previous_observation":changed,
            "previous_observation_revision":"8"})
    }
    fn available(state: Value) -> Value {
        json!({"status":"available","state":state})
    }
    fn all_null_but(fields: Value) -> Value {
        let mut summary = json!({"action":null,"readback":null,"screen":null,"wait":null,
            "settled":null,"signal":null,"repaint":null,"matched":null,"role":null,
            "control_id":null,"exited":null,"claim":null,"release":null});
        for (key, value) in fields.as_object().unwrap() {
            summary[key] = value.clone();
        }
        summary
    }

    #[test]
    fn input_outcomes_and_wait_modes() {
        let signal = json!({"mode":"signal","outcome":"signal","waited_ms":812,"settled":true,
            "signal":{"seq":7,"event":"stop","message":null},"signal_at_ms":700,
            "repaint":{"outcome":"settled","waited_ms":112}});
        let written = json!({"terminal_id":"t1","request_id":"r1","outcome":"written",
            "application_result":"unverified","control_id":"c-root",
            "observation":available(state(signal, json!(true), "observer", Value::Null))});
        let summary = receipt_summary("input", &written);
        assert_eq!(
            summary,
            all_null_but(
                json!({"action":"written","readback":"available","screen":"changed","wait":"signal",
                "settled":true,"signal":"stop","repaint":"settled","role":"observer",
                "control_id":null,"exited":false})
            )
        );
        assert_eq!(
            summary_line("t1", &summary),
            "terminal t1 input written; screen changed settled; wait signal; \
             signal stop, repaint settled; role observer; details in structuredContent"
        );
        let no_signal = json!({"mode":"signal","outcome":"no_signal","waited_ms":15002,"settled":false,
            "signal":null,"signal_at_ms":null,"repaint":null});
        let moved = json!({"outcome":"written",
            "observation":available(state(no_signal, json!(true), "owner", json!("c-1")))});
        let summary = receipt_summary("input", &moved);
        assert_eq!(
            summary["screen"], "changed",
            "the screen fact, not the wait"
        );
        assert_eq!(summary["wait"], "no_signal");
        assert_eq!(summary["settled"], false);
        assert_eq!(summary["signal"], Value::Null);
        assert_eq!(
            summary_line("t1", &summary),
            "terminal t1 input written; screen changed; wait no_signal; role owner; \
             details in structuredContent"
        );
        let change = json!({"mode":"change","outcome":"changed","waited_ms":300,"settled":true,
            "baseline_revision":"8","baseline_signal_seq":0});
        let refused = json!({"outcome":"refused",
            "observation":available(state(change.clone(), json!(true), "owner", json!("c-1")))});
        let summary = receipt_summary("input", &refused);
        assert_eq!(summary["action"], "refused");
        assert_eq!(summary["screen"], "changed");
        assert_eq!(summary["wait"], "changed");
        assert_eq!(summary["settled"], true);
        assert_eq!(
            summary["signal"],
            Value::Null,
            "change mode carries no signal"
        );
        assert_eq!(summary["repaint"], Value::Null);
        assert_eq!(summary["matched"], Value::Null);
        assert_eq!(summary["role"], "owner");
        assert_eq!(summary["control_id"], "c-1");
        assert_eq!(
            summary_line("t1", &summary),
            "terminal t1 input refused; screen changed settled; wait changed; role owner; \
             details in structuredContent"
        );
        let text = json!({"mode":"text","outcome":"matched","waited_ms":1200,"settled":false,
            "text":{"pattern":"❯","row":5,"revision":"9","already":false}});
        let unknown = json!({"outcome":"unknown",
            "observation":available(state(text, json!(true), "owner", json!("c-1")))});
        let summary = receipt_summary("input", &unknown);
        assert_eq!(summary["action"], "unknown");
        assert_eq!(summary["screen"], "changed");
        assert_eq!(summary["wait"], "matched");
        assert_eq!(summary["settled"], false);
        assert_eq!(summary["matched"], "❯");
        assert_eq!(
            summary_line("t1", &summary),
            "terminal t1 input unknown; screen changed; wait matched; matched ❯; role owner; \
             details in structuredContent"
        );
        let elapsed = json!({"mode":"elapsed","outcome":"elapsed","waited_ms":0,"settled":false});
        let mut exited = state(elapsed, json!(false), "observer", Value::Null);
        exited["exited"] = json!(true);
        let stale = json!({"outcome":"stale_observation","observation":available(exited)});
        let summary = receipt_summary("input", &stale);
        assert_eq!(summary["action"], "stale_observation");
        assert_eq!(summary["screen"], "unchanged");
        assert_eq!(summary["wait"], "elapsed");
        assert_eq!(summary["exited"], true);
        assert_eq!(
            summary_line("t1", &summary),
            "terminal t1 input stale_observation; screen unchanged; wait elapsed; \
             role observer exited; details in structuredContent"
        );
        let unavailable = json!({"outcome":"control_unavailable","reason":"why",
            "claim":{"status":"unavailable","reason":"why"},
            "observation":available(state(json!({"outcome":"elapsed","settled":false}),
                json!(false), "observer", Value::Null))});
        let summary = receipt_summary("input", &unavailable);
        assert_eq!(summary["action"], "control_unavailable");
        assert_eq!(summary["claim"], "unavailable");
        assert_eq!(
            summary_line("t1", &summary),
            "terminal t1 input control_unavailable; screen unchanged; wait elapsed; \
             role observer; claim unavailable; details in structuredContent"
        );
        // A readback without the screen fact says null, never a guess from the wait outcome.
        let bare = json!({"outcome":"written",
            "observation":available(state(change, Value::Null, "owner", json!("c-1")))});
        let summary = receipt_summary("input", &bare);
        assert_eq!(summary["screen"], Value::Null);
        assert_eq!(summary["wait"], "changed");
    }

    #[test]
    fn first_observation_on_a_connection_has_no_screen_fact() {
        let elapsed = json!({"mode":"elapsed","outcome":"elapsed","waited_ms":0,"settled":false});
        let mut first = state(elapsed, json!(false), "owner", json!("c-1"));
        first["previous_observation_revision"] = Value::Null;
        let claim = json!({"terminal_id":"t1","connection_id":"n1","control_id":"c-1",
            "observation":available(first)});
        let summary = receipt_summary("claim", &claim);
        assert_eq!(
            summary,
            all_null_but(
                json!({"action":"claim","readback":"available","screen":null,"wait":"elapsed",
                "settled":false,"role":"owner","control_id":"c-1","exited":false})
            ),
            "a fact about nothing is not a fact"
        );
        assert_eq!(
            summary_line("t1", &summary),
            "terminal t1 claim; wait elapsed; role owner; \
             details in structuredContent"
        );
        // `settled` qualifies the screen segment: without one it is not rendered on its own.
        let mut settled = summary.clone();
        settled["settled"] = json!(true);
        assert_eq!(
            summary_line("t1", &settled),
            "terminal t1 claim; wait elapsed; role owner; \
             details in structuredContent"
        );
        // The same readback after a previous observation keeps `unchanged`.
        let mut later = claim.clone();
        later["observation"]["state"]["previous_observation_revision"] = json!("9");
        assert_eq!(receipt_summary("claim", &later)["screen"], "unchanged");
    }

    /// Without a readback every state field is null (the receipt's own `control_id` is never copied).
    #[test]
    fn control_receipts_and_missing_or_unavailable_readbacks() {
        let claim = json!({"terminal_id":"t1","connection_id":"n1","control_id":"c-1",
            "observation":available(state(json!({"outcome":"elapsed","settled":false}),
                json!(false), "owner", json!("c-1")))});
        let summary = receipt_summary("claim", &claim);
        assert_eq!(
            summary,
            all_null_but(
                json!({"action":"claim","readback":"available","screen":"unchanged","wait":"elapsed",
                "settled":false,"role":"owner","control_id":"c-1","exited":false})
            )
        );
        assert_eq!(
            summary_line("t1", &summary),
            "terminal t1 claim; screen unchanged; wait elapsed; role owner; \
             details in structuredContent"
        );
        let release = json!({"terminal_id":"t1","connection_id":"n1","control_id":null});
        let summary = receipt_summary("release", &release);
        assert_eq!(
            summary,
            all_null_but(json!({"action":"release","readback":"none"}))
        );
        assert_eq!(
            summary_line("t1", &summary),
            "terminal t1 release; no readback; details in structuredContent"
        );
        // The receipt's control_id is the lease the claim granted; without a readback the summary
        // must not present it as the current state.
        let bare = json!({"outcome":"written","control_id":"c-lease","claim":{"status":"claimed","control_id":"c-lease"},
            "release":{"status":"requested"}});
        let summary = receipt_summary("input", &bare);
        assert_eq!(summary["readback"], "none");
        assert_eq!(summary["control_id"], Value::Null);
        assert_eq!(summary["role"], Value::Null);
        assert_eq!(summary["claim"], "claimed");
        assert_eq!(summary["release"], "requested");
        assert_eq!(
            summary_line("t1", &summary),
            "terminal t1 input written; no readback; claim claimed; release requested; \
             details in structuredContent"
        );
        // After a release the readback shows no control while the receipt
        // still carries the granted lease: the summary follows the readback.
        let released = json!({"outcome":"written","control_id":"c-lease","claim":{"status":"claimed","control_id":"c-lease"},
            "release":{"status":"released"},
            "observation":available(state(json!({"outcome":"changed","settled":true}),
                json!(true), "observer", Value::Null))});
        let summary = receipt_summary("input", &released);
        assert_eq!(summary["control_id"], Value::Null);
        assert_eq!(summary["role"], "observer");
        assert_eq!(summary["release"], "released");
        let failed =
            json!({"outcome":"written","observation":{"status":"unavailable","reason":"gone"}});
        let summary = receipt_summary("input", &failed);
        assert_eq!(
            summary,
            all_null_but(json!({"action":"written","readback":"unavailable"}))
        );
        assert_eq!(
            summary_line("t1", &summary),
            "terminal t1 input written; readback unavailable; details in structuredContent"
        );
        // A malformed observation block is `none`, never a panic.
        let odd = json!({"outcome":"written","observation":"?"});
        assert_eq!(receipt_summary("input", &odd)["readback"], "none");
        assert_eq!(
            summary_line("t1", &json!({"action":7})),
            "terminal t1 input 7; no readback; details in structuredContent"
        );
    }
}
