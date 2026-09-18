use super::*;

/// #1620 F6 — an image failure after creation, claim and text readback
/// keeps the text state, the claim and the ids, drops the PNG and reports
/// the reason; an image success replaces state and PNG.
#[test]
fn open_image_failure_keeps_the_text_state_and_reports_image_unavailable() {
    let text = json!({"terminal_id":"t-1","text":["READY"],"role":"owner","control_id":"c-1"});
    let mut metadata = text.clone();
    let mut png = Some(vec![1, 2, 3]);
    apply_image_outcome(
        &mut metadata,
        &mut png,
        Err(anyhow::anyhow!("terminal image unavailable: zero geometry")),
    );
    assert!(png.is_none(), "no PNG on an image failure");
    assert_eq!(
        metadata["image"],
        json!({"status":"unavailable","reason":"terminal image unavailable: zero geometry"})
    );
    for key in ["terminal_id", "text", "role", "control_id"] {
        assert_eq!(metadata[key], text[key], "{key} must survive");
    }
    metadata["claim"] = json!({"status":"claimed","control_id":"c-1"});
    metadata["card_id"] = json!("card-1");
    let wire = serde_json::to_value(observation_result(metadata.clone(), png).unwrap()).unwrap();
    assert_eq!(wire["structuredContent"], metadata);
    assert_eq!(wire["content"].as_array().unwrap().len(), 1, "text only");
    assert_eq!(wire["content"][0]["type"], "text");

    let mut metadata = text.clone();
    let mut png = None;
    let rendered =
        json!({"terminal_id":"t-1","text":["READY"],"image_source":"rmux_client_projection"});
    apply_image_outcome(
        &mut metadata,
        &mut png,
        Ok((rendered.clone(), Some(vec![9]))),
    );
    assert_eq!(metadata, rendered);
    assert_eq!(png, Some(vec![9]));
}

/// #1704 S1 — the rule counts join the open's summary line only when the
/// state carries the echoed block, before the structuredContent pointer.
#[test]
fn open_summary_names_the_permission_counts_only_when_the_block_is_echoed() {
    let mut state = json!({"terminal_id":"t-1","observation_id":"o-1","observation_revision":3,
        "role":"owner","cols":80,"rows":24,"cursor":{"row":1,"column":2},
        "wait":{"outcome":"elapsed"}});
    let plain = observation_summary(&state);
    assert_eq!(
        plain,
        "terminal t-1 observation o-1 revision 3 owner 80x24 cursor 1,2 wait elapsed; \
         full state in structuredContent"
    );
    state["claude_permissions"] = json!({"allow":["Edit(//w/**)","Bash(git status *)"],
        "ask":["Bash(git push *)"],"deny":[]});
    assert_eq!(
        observation_summary(&state),
        "terminal t-1 observation o-1 revision 3 owner 80x24 cursor 1,2 wait elapsed \
         permissions allow 2 ask 1 deny 0; full state in structuredContent"
    );
    state["claude_permissions"] = json!(null);
    assert_eq!(observation_summary(&state), plain);
}

/// #1710 — an observe that searched the history names the verdict and the
/// row on the returned screen; the segment is absent without a search.
#[test]
fn observe_summary_names_the_history_search_verdict_only_when_present() {
    let mut state = json!({"terminal_id":"t-1","observation_id":"o-1","observation_revision":3,
        "role":"observer","cols":80,"rows":24,"cursor":{"row":1,"column":2},
        "wait":{"outcome":"elapsed"}});
    let plain = observation_summary(&state);
    assert!(!plain.contains("scroll_to"), "{plain}");
    state["scroll_to"] = json!({"pattern":"MARK","occurrence":"latest","status":"found",
        "row_absolute":30,"row":0});
    assert_eq!(
        observation_summary(&state),
        "terminal t-1 observation o-1 revision 3 observer 80x24 cursor 1,2 wait elapsed \
         scroll_to found row 0; full state in structuredContent"
    );
    state["scroll_to"] = json!({"pattern":"MARK","occurrence":"earliest","status":"not_found",
        "row_absolute":null,"row":null});
    assert_eq!(
        observation_summary(&state),
        "terminal t-1 observation o-1 revision 3 observer 80x24 cursor 1,2 wait elapsed \
         scroll_to not_found; full state in structuredContent"
    );
}

/// #1710 — the invalid-params table of the history search arguments, before
/// any service call.
#[test]
fn scroll_to_request_refuses_the_coupled_and_malformed_shapes() {
    let text = |value: &str| Some(value.to_owned());
    for (name, request, expected) in [
        (
            "occurrence-alone",
            scroll_to_request(None, text("latest"), 0, false),
            "scroll_to_occurrence needs scroll_to_text",
        ),
        (
            "bad-occurrence",
            scroll_to_request(text("x"), text("first"), 0, false),
            "scroll_to_occurrence must be latest or earliest, not first",
        ),
        (
            "empty",
            scroll_to_request(text(""), None, 0, false),
            "scroll_to_text: 1..200 bytes of printable text",
        ),
        (
            "too-long",
            scroll_to_request(Some("x".repeat(201)), None, 0, false),
            "scroll_to_text: 1..200 bytes of printable text",
        ),
        (
            "control-char",
            scroll_to_request(text("a\tb"), None, 0, false),
            "scroll_to_text: 1..200 bytes of printable text",
        ),
        (
            "scrolled",
            scroll_to_request(text("x"), None, 4, false),
            "scroll_to_text needs scroll_offset 0",
        ),
        (
            "text-wait",
            scroll_to_request(text("x"), None, 0, true),
            "scroll_to_text and wait_for=text / text conditions are exclusive",
        ),
    ] {
        let error = request
            .err()
            .unwrap_or_else(|| panic!("{name} must be refused"));
        assert_eq!(error.code, RpcError::INVALID_PARAMS, "{name}: {error}");
        assert_eq!(error.message, expected, "{name}");
    }
    assert_eq!(scroll_to_request(None, None, 4, true).unwrap(), None);
    let latest = scroll_to_request(text("MARK"), None, 0, false)
        .unwrap()
        .unwrap();
    assert_eq!(latest.pattern(), "MARK");
    assert_eq!(latest.occurrence(), Occurrence::Latest);
    let earliest = scroll_to_request(text("MARK"), text("earliest"), 0, false)
        .unwrap()
        .unwrap();
    assert_eq!(earliest.occurrence(), Occurrence::Earliest);
}

#[test]
fn terminal_open_failure_is_a_one_line_summary_with_structured_detail() {
    let receipt = json!({"operation_id":"op-7","outcome":"unavailable","detail":"Failed { error: \"spawn refused\" }"});
    let wire = serde_json::to_value(open_failure_result(receipt.clone())).unwrap();
    assert_eq!(wire["structuredContent"], receipt);
    let content = wire["content"].as_array().unwrap();
    assert_eq!(content.len(), 1);
    assert_eq!(
        content[0]["text"],
        "terminal open unavailable operation op-7; details in structuredContent"
    );
    assert!(
        !content[0]["text"]
            .as_str()
            .unwrap()
            .contains("spawn refused"),
        "the detail must live only in structuredContent"
    );
}
