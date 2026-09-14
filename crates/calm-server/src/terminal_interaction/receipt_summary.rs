//! `summary` on input and control receipts (#1677): a flat digest of facts
//! the receipt already carries several levels down (outcome, readback wait,
//! hook signal, repaint, control state), derived once the readback and
//! release facts are final. No new facts, no fence reads it, and
//! `application_result: "unverified"` stays where it is: the digest is
//! evidence, not a verdict.
use serde_json::{Value, json};

/// The digest of `receipt`. `action` is the control action (`claim` or
/// `release`); an input receipt's `outcome` takes its place. Every field is
/// nullable: the wait fields are the readback's `wait` block (mode-dependent
/// ones are null outside their mode), `role`/`control_id`/`exited` are the
/// readback state's (`control_id` is the readback's, explicit null included,
/// never the receipt's own lease), `claim`/`release` are the receipt's
/// status blocks.
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
    json!({"action":action,"readback":readback,
        "screen":wait["outcome"],"settled":wait["settled"],
        "signal":wait["signal"]["event"],"repaint":wait["repaint"]["outcome"],
        "matched":wait["text"]["pattern"],
        "role":state["role"],"control_id":state["control_id"],"exited":state["exited"],
        "claim":receipt["claim"]["status"],"release":receipt["release"]["status"]})
}

/// The one-line text block saying the same in words, so a client that shows
/// only text gets the digest too.
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
            let settled = if summary["settled"] == true {
                " settled"
            } else {
                ""
            };
            parts.push(format!("screen {}{settled}", word(&summary["screen"])));
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

    fn state(wait: Value, role: &str, control_id: Value) -> Value {
        json!({"observation_id":"o-2","observation_revision":"9","role":role,"control_id":control_id,
            "exited":false,"text":["$ "],"wait":wait})
    }
    fn available(state: Value) -> Value {
        json!({"status":"available","state":state})
    }
    fn all_null_but(fields: Value) -> Value {
        let mut summary = json!({"action":null,"readback":null,"screen":null,"settled":null,
            "signal":null,"repaint":null,"matched":null,"role":null,"control_id":null,
            "exited":null,"claim":null,"release":null});
        for (key, value) in fields.as_object().unwrap() {
            summary[key] = value.clone();
        }
        summary
    }

    /// Every input outcome names itself; the wait fields come from the
    /// readback's `wait` block in its own mode and stay null elsewhere.
    #[test]
    fn input_outcomes_and_wait_modes() {
        let signal = json!({"mode":"signal","outcome":"signal","waited_ms":812,"settled":true,
            "signal":{"seq":7,"event":"stop","message":null},"signal_at_ms":700,
            "repaint":{"outcome":"settled","waited_ms":112}});
        let written = json!({"terminal_id":"t1","request_id":"r1","outcome":"written",
            "application_result":"unverified","control_id":"c-root",
            "observation":available(state(signal, "observer", Value::Null))});
        let summary = receipt_summary("input", &written);
        assert_eq!(
            summary,
            all_null_but(
                json!({"action":"written","readback":"available","screen":"signal",
                "settled":true,"signal":"stop","repaint":"settled","role":"observer",
                "control_id":null,"exited":false})
            )
        );
        assert_eq!(
            summary_line("t1", &summary),
            "terminal t1 input written; screen signal settled; signal stop, repaint settled; \
             role observer; details in structuredContent"
        );
        let change = json!({"mode":"change","outcome":"changed","waited_ms":300,"settled":true,
            "baseline_revision":"8","baseline_signal_seq":0});
        let refused = json!({"outcome":"refused","observation":available(state(change, "owner", json!("c-1")))});
        let summary = receipt_summary("input", &refused);
        assert_eq!(summary["action"], "refused");
        assert_eq!(summary["screen"], "changed");
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
            "terminal t1 input refused; screen changed settled; role owner; details in structuredContent"
        );
        let text = json!({"mode":"text","outcome":"matched","waited_ms":1200,"settled":false,
            "text":{"pattern":"❯","row":5,"revision":"9","already":false}});
        let unknown = json!({"outcome":"unknown","observation":available(state(text, "owner", json!("c-1")))});
        let summary = receipt_summary("input", &unknown);
        assert_eq!(summary["action"], "unknown");
        assert_eq!(summary["screen"], "matched");
        assert_eq!(summary["settled"], false);
        assert_eq!(summary["matched"], "❯");
        assert_eq!(
            summary_line("t1", &summary),
            "terminal t1 input unknown; screen matched; matched ❯; role owner; details in structuredContent"
        );
        let elapsed = json!({"mode":"elapsed","outcome":"elapsed","waited_ms":0,"settled":false});
        let mut exited = state(elapsed, "observer", Value::Null);
        exited["exited"] = json!(true);
        let stale = json!({"outcome":"stale_observation","observation":available(exited)});
        let summary = receipt_summary("input", &stale);
        assert_eq!(summary["action"], "stale_observation");
        assert_eq!(summary["screen"], "elapsed");
        assert_eq!(summary["exited"], true);
        assert_eq!(
            summary_line("t1", &summary),
            "terminal t1 input stale_observation; screen elapsed; role observer exited; details in structuredContent"
        );
        let unavailable = json!({"outcome":"control_unavailable","reason":"why",
            "claim":{"status":"unavailable","reason":"why"},
            "observation":available(state(json!({"outcome":"elapsed","settled":false}), "observer", Value::Null))});
        let summary = receipt_summary("input", &unavailable);
        assert_eq!(summary["action"], "control_unavailable");
        assert_eq!(summary["claim"], "unavailable");
        assert_eq!(
            summary_line("t1", &summary),
            "terminal t1 input control_unavailable; screen elapsed; role observer; \
             claim unavailable; details in structuredContent"
        );
    }

    /// Claim and release receipts name the control action; without a
    /// readback every state field is null (the receipt's own `control_id`
    /// is never copied); an unavailable readback says so.
    #[test]
    fn control_receipts_and_missing_or_unavailable_readbacks() {
        let claim = json!({"terminal_id":"t1","connection_id":"n1","control_id":"c-1",
            "observation":available(state(json!({"outcome":"elapsed","settled":false}), "owner", json!("c-1")))});
        let summary = receipt_summary("claim", &claim);
        assert_eq!(
            summary,
            all_null_but(
                json!({"action":"claim","readback":"available","screen":"elapsed",
                "settled":false,"role":"owner","control_id":"c-1","exited":false})
            )
        );
        assert_eq!(
            summary_line("t1", &summary),
            "terminal t1 claim; screen elapsed; role owner; details in structuredContent"
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
        // The receipt's control_id is the lease the claim granted; without a
        // readback the summary must not present it as the current state.
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
            "observation":available(state(json!({"outcome":"changed","settled":true}), "observer", Value::Null))});
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
