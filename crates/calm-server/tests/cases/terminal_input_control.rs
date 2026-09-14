//! #1666 S3 — `input claim:true` / `release:true`: control per scenario
//! through the real MCP tools, renderer and PTY, with a real human client
//! where a takeover is needed.
use crate::terminal_support::{Harness, human_takeover};
use serde_json::{Value, json};
use std::time::Duration;

const COUNT_PROBE: &str = "i=0; printf 'READY\\n'; while IFS= read -r line; do i=$((i+1)); printf x >> physical-lines; printf 'COUNT:%s:%s\\n' \"$i\" \"$line\"; done";

fn receipt(response: &Value) -> &Value {
    assert!(response.get("error").is_none(), "{response}");
    &response["result"]["structuredContent"]
}
fn observation(response: &Value) -> &Value {
    let result = receipt(response);
    assert_eq!(result["observation"]["status"], "available", "{result}");
    &result["observation"]["state"]
}
fn summary(response: &Value) -> &str {
    response["result"]["content"][0]["text"].as_str().unwrap()
}
fn error_text(response: &Value) -> String {
    response["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("expected an error: {response}"))
        .to_owned()
}
fn has_line(state: &Value, needle: &str) -> bool {
    state["text"]
        .as_array()
        .unwrap()
        .iter()
        .any(|line| line.as_str().unwrap().contains(needle))
}
fn revision(state: &Value) -> u64 {
    state["observation_revision"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap()
}
fn live_rows(h: &Harness, terminal: &str) -> Vec<String> {
    h.state
        .terminal_renderer
        .get(terminal)
        .unwrap()
        .handle
        .model_view
        .lock()
        .unwrap()
        .capture(0)
        .unwrap()
        .0
        .text
}
async fn wait_for_live_line(h: &Harness, terminal: &str, needle: &str) {
    let start = std::time::Instant::now();
    while !live_rows(h, terminal)
        .iter()
        .any(|row| row.contains(needle))
    {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "{needle} never painted"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}
fn registry_owner(h: &Harness, terminal: &str) -> Option<uuid::Uuid> {
    h.state
        .terminal_renderer
        .get(terminal)
        .unwrap()
        .handle
        .owner_registry
        .lock()
        .unwrap()
        .current_owner()
}
/// Open the count probe WITHOUT a claim and observe it as observer.
async fn open_observed(h: &Harness, request: &str) -> (String, Value) {
    let terminal = h
        .ok(
            "calm.terminal.open",
            json!({"program":COUNT_PROBE,"request_id":request}),
        )
        .await["terminal_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ready = h.observe_text(&terminal, "READY").await;
    assert_eq!(ready["role"], "observer", "{ready}");
    (terminal, ready)
}
fn submit(terminal: &str, request: &str, text: &str, extra: Value) -> Value {
    let mut args = json!({"terminal_id":terminal,"request_id":request,"action":{"type":"submit","text":text},
        "observe":true,"wait_for":"change","wait_ms":3000});
    for (key, value) in extra.as_object().unwrap() {
        args[key] = value.clone();
    }
    args
}
fn physical_lines(h: &Harness) -> Vec<u8> {
    std::fs::read(h.root.path().join("physical-lines")).unwrap_or_default()
}

/// A free terminal: the first observed input claims and writes in one call,
/// the receipt names the claim and the new control id, the readback is the
/// owner's; a second `claim:true` is a no-op `held`; a replay claims and
/// writes nothing more.
#[tokio::test]
async fn input_claim_grants_when_free_and_writes_with_the_claim_in_the_receipt() {
    let h = Harness::start().await;
    let (terminal, _ready) = open_observed(&h, "claim-free").await;
    let before = h.interaction().input_ack_sequence(&terminal).await.unwrap();
    let first = h
        .call(
            "calm.terminal.input",
            submit(&terminal, "first", "hello", json!({"claim":true})),
        )
        .await;
    let written = receipt(&first);
    assert_eq!(written["outcome"], "written", "{first}");
    assert_eq!(written["claim"]["status"], "claimed", "{written}");
    let control = written["claim"]["control_id"]
        .as_str()
        .expect("the claim names its control id")
        .to_owned();
    assert_eq!(written["control_id"], control, "{written}");
    assert_eq!(written["application_result"], "unverified");
    let state = observation(&first);
    assert_eq!(state["role"], "owner", "{state}");
    assert_eq!(state["control_id"], control);
    assert!(has_line(state, "COUNT:1:hello"), "{state}");
    assert_eq!(
        summary(&first),
        format!(
            "terminal {terminal} input written readback available; details in structuredContent"
        )
    );
    // Held: no second claim, the same lease, the ordinary fences.
    let second = h
        .call(
            "calm.terminal.input",
            submit(&terminal, "second", "again", json!({"claim":true})),
        )
        .await;
    assert_eq!(receipt(&second)["outcome"], "written", "{second}");
    assert_eq!(
        receipt(&second)["claim"],
        json!({"status":"held"}),
        "{second}"
    );
    assert!(has_line(observation(&second), "COUNT:2:again"));
    assert_eq!(observation(&second)["control_id"], control, "same lease");
    // A replay of the claiming request: same receipt, fresh readback, no
    // claim and no write.
    let replay = h
        .call(
            "calm.terminal.input",
            submit(
                &terminal,
                "first",
                "hello",
                json!({"claim":true,"wait_for":"elapsed","wait_ms":200}),
            ),
        )
        .await;
    assert_eq!(receipt(&replay)["claim"]["status"], "claimed");
    assert_eq!(receipt(&replay)["control_id"], control);
    assert!(!has_line(observation(&replay), "COUNT:3"), "{replay}");
    assert_eq!(
        h.interaction().input_ack_sequence(&terminal).await,
        Some(before + 2)
    );
    assert_eq!(physical_lines(&h), b"xx");
    // `claim` is part of the fingerprint.
    let toggled = h
        .call(
            "calm.terminal.input",
            submit(&terminal, "first", "hello", json!({})),
        )
        .await;
    assert!(
        error_text(&toggled).contains("reused with different arguments"),
        "{toggled}"
    );
    h.stop(&terminal).await;
}

/// A human holds control: the claim is refused by the pump (never a
/// takeover), nothing is written or cached, the result is structured with
/// a fresh observer observation; once the human is gone the same request
/// claims and writes.
#[tokio::test]
async fn input_claim_is_refused_while_a_human_holds_control_and_writes_nothing() {
    let h = Harness::start().await;
    let (terminal, _ready) = open_observed(&h, "claim-human").await;
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    let user = uuid::Uuid::new_v4();
    let (pump, _incoming) = human_takeover(&entry, &terminal, user).await;
    let before = h.interaction().input_ack_sequence(&terminal).await.unwrap();
    let refused = h
        .call(
            "calm.terminal.input",
            submit(&terminal, "hi", "hi", json!({"claim":true})),
        )
        .await;
    let result = receipt(&refused);
    assert_eq!(result["outcome"], "control_unavailable", "{result}");
    assert_eq!(result["reason"], "terminal is controlled by another client");
    assert_eq!(result["claim"]["status"], "unavailable", "{result}");
    assert_eq!(result["application_result"], "unverified");
    assert_eq!(result["request_id"], "hi");
    assert!(result.get("control_id").is_none());
    assert!(
        result["next"]
            .as_str()
            .unwrap()
            .contains("nothing was written")
    );
    let state = observation(&refused);
    assert_eq!(state["role"], "observer", "{state}");
    assert_eq!(state["control_id"], Value::Null);
    assert!(!has_line(state, "COUNT:1"), "{state}");
    assert_eq!(
        summary(&refused),
        format!(
            "terminal {terminal} input control_unavailable readback available; details in structuredContent"
        )
    );
    assert_eq!(
        registry_owner(&h, &terminal),
        Some(user),
        "the human keeps control"
    );
    assert_eq!(
        h.interaction().input_ack_sequence(&terminal).await,
        Some(before)
    );
    assert!(!h.interaction().input_pending(&terminal).await);
    assert!(physical_lines(&h).is_empty());
    // Nothing was cached: the same request_id with other arguments is not a
    // conflict (and is refused again while the human holds control).
    let again = h
        .call(
            "calm.terminal.input",
            submit(&terminal, "hi", "hi there", json!({"claim":true})),
        )
        .await;
    assert_eq!(receipt(&again)["outcome"], "control_unavailable", "{again}");
    pump.abort();
    let start = std::time::Instant::now();
    while registry_owner(&h, &terminal).is_some() {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "lease not released"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let claimed = h
        .call(
            "calm.terminal.input",
            submit(&terminal, "hi", "hi", json!({"claim":true})),
        )
        .await;
    assert_eq!(receipt(&claimed)["outcome"], "written", "{claimed}");
    assert_eq!(receipt(&claimed)["claim"]["status"], "claimed");
    assert!(has_line(observation(&claimed), "COUNT:1:hi"));
    assert_eq!(observation(&claimed)["role"], "owner");
    h.stop(&terminal).await;
}

/// The observation named a lease this connection no longer holds (released
/// since): `claim:true` does not claim on that observation; a fresh observer
/// observation does.
#[tokio::test]
async fn input_claim_is_refused_when_the_observation_named_a_lost_lease() {
    let h = Harness::start().await;
    let (terminal, _ready) = open_observed(&h, "claim-lost").await;
    let claimed = h
        .call(
            "calm.terminal.control",
            json!({"terminal_id":terminal,"action":"claim","observe":true}),
        )
        .await;
    let owner_view = observation(&claimed).clone();
    assert_eq!(owner_view["role"], "owner");
    h.ok(
        "calm.terminal.control",
        json!({"terminal_id":terminal,"action":"release"}),
    )
    .await;
    let refused = h
        .call(
            "calm.terminal.input",
            submit(
                &terminal,
                "lost",
                "x",
                json!({"claim":true,"observation_id":owner_view["observation_id"]}),
            ),
        )
        .await;
    let result = receipt(&refused);
    assert_eq!(result["outcome"], "control_unavailable", "{result}");
    assert_eq!(
        result["reason"],
        "terminal control held at the observation is no longer held; observe before input"
    );
    assert_eq!(result["claim"]["status"], "unavailable");
    assert_eq!(registry_owner(&h, &terminal), None, "no claim was sent");
    assert!(physical_lines(&h).is_empty());
    // The fresh observation is an observer's: the claim now applies.
    let fresh = observation(&refused);
    assert_eq!(fresh["role"], "observer");
    let claimed = h
        .call(
            "calm.terminal.input",
            submit(
                &terminal,
                "lost",
                "x",
                json!({"claim":true,"observation_id":fresh["observation_id"]}),
            ),
        )
        .await;
    assert_eq!(receipt(&claimed)["outcome"], "written", "{claimed}");
    assert_eq!(receipt(&claimed)["claim"]["status"], "claimed");
    // Without `claim:true` an observer observation stays the ordinary error.
    let plain = h
        .call(
            "calm.terminal.input",
            submit(
                &terminal,
                "plain",
                "y",
                json!({"observation_id":fresh["observation_id"]}),
            ),
        )
        .await;
    assert!(
        error_text(&plain).contains("terminal control changed; observe before input"),
        "{plain}"
    );
    h.stop(&terminal).await;
}

/// #1620 R6 shape on input: the grant is folded with a human takeover
/// (delivery on the Planner's connection is held from inside the claim
/// window until the human has taken over). The pump's verdict is Granted,
/// the applied state names the human: a refusal without a write, well
/// before the 7 s budget.
#[tokio::test]
async fn input_claim_folded_with_a_takeover_is_refused_without_a_write() {
    use std::sync::{Arc, Mutex};
    let h = Harness::start().await;
    let (terminal, _ready) = open_observed(&h, "claim-folded").await;
    let user = uuid::Uuid::new_v4();
    type HumanClient = (
        tokio::task::AbortHandle,
        tokio::sync::mpsc::Sender<calm_session::ClientMsg>,
    );
    let human: Arc<Mutex<Option<HumanClient>>> = Arc::new(Mutex::new(None));
    let seam_human = human.clone();
    let renderer = h.state.terminal_renderer.clone();
    let service = h.interaction();
    h.interaction()
        .set_claim_window_seam(Box::new(move |terminal_id: String| {
            Box::pin(async move {
                let held = service
                    .hold_delivery(&terminal_id)
                    .await
                    .expect("the observe established the Planner's client");
                let entry = renderer.get(&terminal_id).unwrap();
                tokio::spawn(async move {
                    let start = std::time::Instant::now();
                    while entry
                        .handle
                        .owner_registry
                        .lock()
                        .unwrap()
                        .current_owner()
                        .is_none()
                    {
                        assert!(
                            start.elapsed() < Duration::from_secs(5),
                            "grant never landed"
                        );
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    let client = human_takeover(&entry, &terminal_id, user).await;
                    *seam_human.lock().unwrap() = Some(client);
                    drop(held);
                });
            })
        }));
    let started = std::time::Instant::now();
    let refused = h
        .call(
            "calm.terminal.input",
            submit(&terminal, "folded", "x", json!({"claim":true})),
        )
        .await;
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the claim must not idle to its budget"
    );
    assert!(human.lock().unwrap().is_some(), "the human took over");
    let result = receipt(&refused);
    assert_eq!(result["outcome"], "control_unavailable", "{result}");
    assert_eq!(
        result["reason"],
        "terminal control was taken by another client"
    );
    assert_eq!(result["claim"]["status"], "unavailable");
    assert_eq!(observation(&refused)["role"], "observer");
    assert_eq!(
        registry_owner(&h, &terminal),
        Some(user),
        "the human keeps control"
    );
    assert!(physical_lines(&h).is_empty(), "nothing written");
    assert!(!h.interaction().input_pending(&terminal).await);
    if let Some((pump, _incoming)) = human.lock().unwrap().take() {
        pump.abort();
    }
    h.stop(&terminal).await;
}

/// A granted claim followed by the stale fence: nothing is written, but the
/// stale result says the connection now holds control; the resend is `held`.
#[tokio::test]
async fn input_claim_then_stale_fence_reports_the_claim() {
    let h = Harness::start().await;
    let terminal = h
        .ok(
            "calm.terminal.open",
            json!({"program":"printf 'READY\\n'; while [ ! -e go ]; do sleep 0.02; done; printf 'STATUS_LINE\\n'; cat >/dev/null","request_id":"claim-stale"}),
        )
        .await["terminal_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let ready = h.observe_text(&terminal, "READY").await;
    assert_eq!(ready["role"], "observer");
    std::fs::write(h.root.path().join("go"), b"x").unwrap();
    wait_for_live_line(&h, &terminal, "STATUS_LINE").await;
    let response = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"observation_id":ready["observation_id"],"request_id":"typed","action":{"type":"text","text":"abc"},"claim":true}),
        )
        .await;
    let stale = receipt(&response);
    assert_eq!(stale["outcome"], "stale_observation", "{stale}");
    assert_eq!(stale["claim"]["status"], "claimed", "{stale}");
    let control = stale["claim"]["control_id"].as_str().unwrap().to_owned();
    assert_eq!(stale["control_id"], control);
    assert_eq!(
        stale["observed_revision"].as_u64().unwrap(),
        revision(&ready)
    );
    let fresh = observation(&response);
    assert_eq!(fresh["role"], "owner", "{fresh}");
    assert_eq!(fresh["control_id"], control);
    assert!(has_line(fresh, "STATUS_LINE"));
    assert!(!h.interaction().input_pending(&terminal).await);
    assert!(
        !live_rows(&h, &terminal)
            .iter()
            .any(|row| row.contains("abc"))
    );
    // Resend as advised on the fresh (owner) observation: `held`, written.
    let resent = h
        .call(
            "calm.terminal.input",
            json!({"terminal_id":terminal,"request_id":"typed","action":{"type":"text","text":"abc"},"claim":true,"observe":true,"wait_for":"change","wait_ms":3000}),
        )
        .await;
    assert_eq!(receipt(&resent)["outcome"], "written", "{resent}");
    assert_eq!(receipt(&resent)["claim"], json!({"status":"held"}));
    assert!(has_line(observation(&resent), "abc"));
    h.stop(&terminal).await;
}

/// `release:true` releases after the write: the bytes reached the program,
/// the receipt says released and the readback is an observer's. A replay
/// never releases again (control claimed since stays), and claim+release in
/// one call brackets a one-input scenario.
#[tokio::test]
async fn input_release_releases_after_the_write_and_reads_back_as_observer() {
    let h = Harness::start().await;
    let (terminal, _ready) = open_observed(&h, "release").await;
    h.ok(
        "calm.terminal.control",
        json!({"terminal_id":terminal,"action":"claim","observe":true}),
    )
    .await;
    let before = h.interaction().input_ack_sequence(&terminal).await.unwrap();
    let last = h
        .call(
            "calm.terminal.input",
            submit(&terminal, "bye", "bye", json!({"release":true})),
        )
        .await;
    let written = receipt(&last);
    assert_eq!(written["outcome"], "written", "{last}");
    assert_eq!(
        written["release"],
        json!({"status":"released"}),
        "{written}"
    );
    assert!(written.get("claim").is_none());
    let state = observation(&last);
    assert_eq!(state["role"], "observer", "{state}");
    assert_eq!(state["control_id"], Value::Null);
    assert!(
        has_line(state, "COUNT:1:bye"),
        "written before the release: {state}"
    );
    assert!(
        state["text"].is_array(),
        "input readbacks always carry text"
    );
    assert_eq!(registry_owner(&h, &terminal), None);
    assert_eq!(
        h.interaction().input_ack_sequence(&terminal).await,
        Some(before + 1)
    );
    // The next plain input has only an observer observation to name.
    let plain = h
        .call(
            "calm.terminal.input",
            submit(&terminal, "after", "x", json!({})),
        )
        .await;
    assert!(
        error_text(&plain).contains("terminal control changed; observe before input"),
        "{plain}"
    );
    // A replay after control was claimed again: the cached receipt is
    // returned unchanged, the readback says owner, nothing is released.
    h.ok(
        "calm.terminal.control",
        json!({"terminal_id":terminal,"action":"claim"}),
    )
    .await;
    let replay = h
        .call(
            "calm.terminal.input",
            submit(
                &terminal,
                "bye",
                "bye",
                json!({"release":true,"wait_for":"elapsed","wait_ms":200}),
            ),
        )
        .await;
    assert_eq!(receipt(&replay)["release"], json!({"status":"released"}));
    assert_eq!(receipt(&replay)["outcome"], "written");
    assert_eq!(observation(&replay)["role"], "owner", "{replay}");
    assert!(
        registry_owner(&h, &terminal).is_some(),
        "the replay never releases"
    );
    assert_eq!(
        h.interaction().input_ack_sequence(&terminal).await,
        Some(before + 1)
    );
    assert_eq!(physical_lines(&h), b"x");
    // `release` is part of the fingerprint.
    let toggled = h
        .call(
            "calm.terminal.input",
            submit(&terminal, "bye", "bye", json!({})),
        )
        .await;
    assert!(error_text(&toggled).contains("reused with different arguments"));
    // One-input scenario: claim, write, release in one call.
    h.ok(
        "calm.terminal.control",
        json!({"terminal_id":terminal,"action":"release"}),
    )
    .await;
    let fresh = h
        .ok("calm.terminal.observe", json!({"terminal_id":terminal}))
        .await;
    assert_eq!(fresh["role"], "observer");
    let both = h
        .call(
            "calm.terminal.input",
            submit(
                &terminal,
                "solo",
                "solo",
                json!({"claim":true,"release":true}),
            ),
        )
        .await;
    assert_eq!(receipt(&both)["outcome"], "written", "{both}");
    assert_eq!(receipt(&both)["claim"]["status"], "claimed");
    assert_eq!(receipt(&both)["release"], json!({"status":"released"}));
    assert_eq!(observation(&both)["role"], "observer");
    assert!(has_line(observation(&both), "COUNT:2:solo"));
    assert_eq!(registry_owner(&h, &terminal), None);
    h.stop(&terminal).await;
}

/// A human takes over between the write and the release: the write stands
/// (written before the takeover), the release reports `not_held`, the
/// human keeps control and the readback is an observer's.
#[tokio::test]
async fn input_release_after_a_takeover_reports_not_held() {
    let h = Harness::start().await;
    let (terminal, _ready) = open_observed(&h, "release-takeover").await;
    h.ok(
        "calm.terminal.control",
        json!({"terminal_id":terminal,"action":"claim","observe":true}),
    )
    .await;
    let entry = h.state.terminal_renderer.get(&terminal).unwrap();
    // Hold delivery on the Planner's connection: the write is applied by
    // the pump and reaches the program, but its ack (and every later
    // OwnerChanged) waits behind the gate until the human has taken over.
    let held = h.interaction().hold_delivery(&terminal).await.unwrap();
    let user = uuid::Uuid::new_v4();
    let takeover = async {
        wait_for_live_line(&h, &terminal, "COUNT:1:bye").await;
        let client = human_takeover(&entry, &terminal, user).await;
        drop(held);
        client
    };
    let (response, (pump, _incoming)) = tokio::join!(
        h.call(
            "calm.terminal.input",
            submit(
                &terminal,
                "bye",
                "bye",
                json!({"release":true,"wait_for":"elapsed","wait_ms":100})
            )
        ),
        takeover
    );
    let result = receipt(&response);
    assert_eq!(result["outcome"], "written", "{result}");
    assert_eq!(result["release"], json!({"status":"not_held"}), "{result}");
    assert_eq!(
        registry_owner(&h, &terminal),
        Some(user),
        "the human keeps control"
    );
    let state = observation(&response);
    assert_eq!(state["role"], "observer", "{state}");
    assert!(has_line(state, "COUNT:1:bye"));
    pump.abort();
    h.stop(&terminal).await;
}
