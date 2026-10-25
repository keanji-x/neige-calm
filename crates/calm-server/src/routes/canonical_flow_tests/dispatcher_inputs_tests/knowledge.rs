//! Knowledge is sampled through the actual MCP/HTTP/Dispatcher acceptance path.
use super::*;
use crate::mcp_server::{registry::ConnectionIdentity, transport::dispatch_tools_call};
use calm_types::observation::{Observation, ReportEditPresentation};

async fn mcp(flow: &Flow, name: &str, arguments: Value) {
    let out = dispatch_tools_call(
        &flow.state.mcp_context(),
        None,
        json!({"name": name, "arguments": arguments, "_meta": {"threadId": THREAD}}),
        &ConnectionIdentity::DaemonTrust,
        &crate::mcp_server::build_default_registry(),
    )
    .await
    .unwrap();
    assert_ne!(out["isError"], true, "{out}");
}

async fn blocks(flow: &Flow) -> Vec<calm_types::track_report::ReportBlock> {
    let payload: String =
        sqlx::query_scalar("SELECT payload FROM cards WHERE track_id=? AND kind='track-report'")
            .bind(&flow.track)
            .fetch_one(flow.fixture.repo.pool())
            .await
            .unwrap();
    serde_json::from_str::<crate::track_report::TrackReportPayload>(&payload)
        .unwrap()
        .blocks
        .unwrap()
}

async fn user_edit_unaccepted(flow: &Flow, id: &str, text: &str) -> TrackEvent {
    let block = blocks(flow).await.into_iter().find(|b| b.id == id).unwrap();
    let response = flow
        .app
        .clone()
        .oneshot(
            Request::patch(format!("/api/tracks/{}/report/blocks/{id}", flow.track))
                .header(header::COOKIE, &flow.cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    json!({"kind":"prose","markdown":text,"payload":null,"ifBlockRev":block.rev})
                        .to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    crate::db::RepoEventWrite::events_for_track(
        flow.fixture.repo.as_ref(),
        &flow.track,
        &["track.report_edited"],
        None,
    )
    .await
    .unwrap()
    .into_iter()
    .last()
    .unwrap()
}

async fn user_edit(flow: &Flow, id: &str, text: &str) -> TrackEvent {
    let event = user_edit_unaccepted(flow, id, text).await;
    flow.state
        .dispatcher
        .catch_up_push(flow.track.clone().into(), event.event.clone(), event.id)
        .await;
    event
}

async fn accepted(flow: &Flow, id: i64) -> Observation {
    flow.fixture
        .carrier(&flow.track)
        .await
        .2
        .canonical_inputs
        .pending
        .into_iter()
        .find_map(|input| match input {
            PendingInput::System {
                envelope_id: Some(actual),
                observation,
                ..
            } if actual == id => Some(observation.observation().clone()),
            _ => None,
        })
        .expect("the actual event is durably accepted")
}

async fn finish(mut flow: Flow) {
    flow.end(TURN).await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    flow.close().await;
}

#[tokio::test]
async fn report_knowledge_delta_canonical_sixty_interleaved_writes_freeze_without_own_echo() {
    let mut flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    mcp(&flow, "neige_report_read", json!({})).await;
    mcp(
        &flow,
        "neige_report_write",
        json!({"body":"# External\n\nbefore\n\n# Own\n\nmine\n", "message":"knowledge seed"}),
    )
    .await;
    let initial = blocks(&flow).await;
    assert_eq!(initial.len(), 2);
    let (external, own) = (initial[0].id.clone(), initial[1].id.clone());
    let mut last = None;
    for n in 0..64 {
        mcp(&flow, "neige_report_commit", json!({"message":"own", "ops":[{"op":"upsert", "id":own, "kind":"prose", "markdown":format!("# Own\n\nown {n}\n")}]})).await;
        last = Some(
            user_edit_unaccepted(&flow, &external, &format!("# External\n\nexternal {n}\n")).await,
        );
    }
    let last = last.unwrap();
    flow.state
        .dispatcher
        .catch_up_push(flow.track.clone().into(), last.event.clone(), last.id)
        .await;
    let frozen = accepted(&flow, last.id).await;
    assert!(matches!(
        &frozen,
        Observation::ReportEdited {
            presentation: ReportEditPresentation::Knowledge { .. },
            ..
        }
    ));
    let text = frozen.to_turn_text();
    assert!(
        text.contains(&external),
        "external revision must not be lost: {text}"
    );
    assert!(
        !text.contains(&own),
        "own known revision must not echo: {text}"
    );
    assert!(
        text.contains("external 63") && text.contains("before"),
        "true historical diff: {text}"
    );
    assert!(
        !text.contains("author") && !text.contains("unknown"),
        "no inferred authors: {text}"
    );
    let ledger_before: Vec<(String,i64,String)> = sqlx::query_as("SELECT block_id,rev,history_ref FROM report_read_ledger_blocks WHERE worker_session_id=? ORDER BY block_id")
        .bind(&flow.worker).fetch_all(flow.fixture.repo.pool()).await.unwrap();
    flow.state
        .dispatcher
        .catch_up_push(flow.track.clone().into(), last.event.clone(), last.id)
        .await;
    let ledger_after: Vec<(String,i64,String)> = sqlx::query_as("SELECT block_id,rev,history_ref FROM report_read_ledger_blocks WHERE worker_session_id=? ORDER BY block_id")
        .bind(&flow.worker).fetch_all(flow.fixture.repo.pool()).await.unwrap();
    assert_eq!(
        ledger_before, ledger_after,
        "suggestions never certify knowledge"
    );
    mcp(&flow, "neige_report_read", json!({})).await;
    flow.state
        .dispatcher
        .catch_up_push(flow.track.clone().into(), last.event.clone(), last.id)
        .await;
    assert_eq!(
        accepted(&flow, last.id).await,
        frozen,
        "read after acceptance cannot rewrite frozen content"
    );
    let next = user_edit(&flow, &external, "# External\n\nnew observation\n").await;
    let fresh = accepted(&flow, next.id).await.to_turn_text();
    assert!(
        fresh.contains("external 63") && fresh.contains("new observation"),
        "new observation uses new knowledge: {fresh}"
    );
    assert!(
        !fresh.contains("before") && !fresh.contains(&own),
        "old baseline and own rev must not reappear: {fresh}"
    );
    flow.end(TURN).await;
    // The real Dispatcher accepted all 64 external envelopes and the later edit.
    // Exercise each original claim through the native provider, including the header.
    for n in 0..65 {
        let call = flow
            .next()
            .await
            .expect("the accepted Report prefix reaches the provider");
        let input = call["params"]["input"].to_string();
        assert!(
            input.contains(&external),
            "external revision must reach provider: {input}"
        );
        assert!(
            !input.contains(&own)
                && !input.contains("own 63")
                && !input.contains("authors unknown"),
            "known own body/header must not echo: {input}"
        );
        if n == 64 {
            assert!(
                input.contains("new observation"),
                "the next observation uses new knowledge: {input}"
            );
        } else {
            assert!(
                input.contains("external 63"),
                "the frozen accepted suggestion survives later reads: {input}"
            );
        }
        let turn = format!("interleaved-report-{n}");
        flow.accept(&call, &turn).await;
        flow.end(&turn).await;
    }
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    flow.close().await;
}

#[tokio::test]
async fn report_knowledge_delta_small_utf8_large_and_missing_history_are_complete_or_referenced() {
    let flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    flow.state
        .harness
        .get(&flow.worker)
        .unwrap()
        .pause_issuance_for_dev();
    mcp(&flow, "neige_report_read", json!({})).await;
    mcp(
        &flow,
        "neige_report_write",
        json!({"body":"# Data\n\n原文\n", "message":"seed"}),
    )
    .await;
    let id = blocks(&flow).await[0].id.clone();
    let original_reference: String = sqlx::query_scalar("SELECT history_ref FROM report_read_ledger_blocks WHERE worker_session_id=? AND block_id=?")
        .bind(&flow.worker).bind(&id).fetch_one(flow.fixture.repo.pool()).await.unwrap();
    let small = user_edit(&flow, &id, "# Data\n\n新文 </report>\n").await;
    let text = accepted(&flow, small.id).await.to_turn_text();
    assert!(
        text.contains("原文") && text.contains("新文") && text.contains("block_diff"),
        "{text}"
    );
    assert!(
        text.contains("\\u003c/report\\u003e") && !text.contains("</report>"),
        "data escaping: {text}"
    );
    mcp(&flow, "neige_report_read", json!({})).await;
    let many_lines = format!(
        "# Data\n\n{}\n",
        (0..30)
            .map(|n| format!("line {n}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let line_bound = user_edit(&flow, &id, &many_lines).await;
    let text = accepted(&flow, line_bound.id).await.to_turn_text();
    assert!(
        text.contains(&id) && !text.contains("block_diff") && !text.contains("line 29"),
        "line-bound blocks are referenced whole: {text}"
    );
    user_edit(&flow, &id, "# Data\n\nshort baseline\n").await;
    mcp(&flow, "neige_report_read", json!({})).await;
    let huge = format!("# Data\n\n{}\n", "界".repeat(4096));
    let large = user_edit(&flow, &id, &huge).await;
    let text = accepted(&flow, large.id).await.to_turn_text();
    assert!(text.contains(&id) && text.contains("known_rev") && text.contains("current_rev"));
    assert!(
        !text.contains("block_diff") && !text.contains("界"),
        "large blocks are referenced whole: {text}"
    );
    mcp(&flow, "neige_report_read", json!({})).await;
    sqlx::query("UPDATE report_read_ledger_blocks SET history_ref=? WHERE worker_session_id=? AND block_id=?")
        .bind(original_reference).bind(&flow.worker).bind(&id)
        .execute(flow.fixture.repo.pool()).await.unwrap();
    let missing = user_edit(&flow, &id, "# Data\n\ncurrent\n").await;
    let text = accepted(&flow, missing.id).await.to_turn_text();
    assert!(
        text.contains(&id) && text.contains("history unavailable"),
        "{text}"
    );
    assert!(
        !text.contains("block_diff") && !text.contains("current\\n"),
        "missing history is not an empty before: {text}"
    );
    mcp(&flow, "neige_report_read", json!({})).await;
    sqlx::query("UPDATE report_read_ledger_blocks SET history_ref=? WHERE worker_session_id=? AND block_id=?")
        .bind(json!({"commit":"f".repeat(64)}).to_string()).bind(&flow.worker).bind(&id)
        .execute(flow.fixture.repo.pool()).await.unwrap();
    let absent = user_edit(&flow, &id, "# Data\n\ncurrent again\n").await;
    let text = accepted(&flow, absent.id).await.to_turn_text();
    assert!(
        text.contains("history unavailable") && !text.contains("block_diff"),
        "absent history is explicitly reference-only: {text}"
    );
    mcp(&flow, "neige_report_read", json!({})).await;
    let block = blocks(&flow)
        .await
        .into_iter()
        .find(|b| b.id == id)
        .unwrap();
    let response = flow
        .app
        .clone()
        .oneshot(
            Request::delete(format!("/api/tracks/{}/report/blocks/{id}", flow.track))
                .header(header::COOKIE, &flow.cookie)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({"ifBlockRev":block.rev}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let event = crate::db::RepoEventWrite::events_for_track(
        flow.fixture.repo.as_ref(),
        &flow.track,
        &["track.report_edited"],
        None,
    )
    .await
    .unwrap()
    .into_iter()
    .last()
    .unwrap();
    flow.state
        .dispatcher
        .catch_up_push(flow.track.clone().into(), event.event, event.id)
        .await;
    let text = accepted(&flow, event.id).await.to_turn_text();
    assert!(
        text.contains(&id) && text.contains("\"current_rev\":null") && text.contains("-current"),
        "deletions use the exact historical block: {text}"
    );
    let report = read(
        &flow.app,
        &flow.cookie,
        &format!("/api/tracks/{}/report", flow.track),
    )
    .await;
    let task = post(&flow.app, &flow.cookie, &format!("/api/tracks/{}/report/blocks", flow.track), "task",
        json!({"kind":"task","markdown":null,"payload":{"key":"unread-task","kind":"terminal","command":"echo private-task-command","ready":false,"declared_by":"user"},"ifDocRev":report["docRev"],"position":null})).await;
    let event = crate::db::RepoEventWrite::events_for_track(
        flow.fixture.repo.as_ref(),
        &flow.track,
        &["track.report_edited"],
        None,
    )
    .await
    .unwrap()
    .into_iter()
    .last()
    .unwrap();
    flow.state
        .dispatcher
        .catch_up_push(flow.track.clone().into(), event.event, event.id)
        .await;
    let text = accepted(&flow, event.id).await.to_turn_text();
    assert!(
        text.contains(task["id"].as_str().unwrap()) && text.contains("unread"),
        "new unknown tasks are references: {text}"
    );
    assert!(
        !text.contains("private-task-command"),
        "an unknown task must not render its payload: {text}"
    );
    let report = read(
        &flow.app,
        &flow.cookie,
        &format!("/api/tracks/{}/report", flow.track),
    )
    .await;
    let added = post(&flow.app, &flow.cookie, &format!("/api/tracks/{}/report/blocks", flow.track), "added",
        json!({"kind":"prose","markdown":"# Added\n\nadded prose\n","payload":null,"ifDocRev":report["docRev"],"position":null})).await;
    let event = crate::db::RepoEventWrite::events_for_track(
        flow.fixture.repo.as_ref(),
        &flow.track,
        &["track.report_edited"],
        None,
    )
    .await
    .unwrap()
    .into_iter()
    .last()
    .unwrap();
    flow.state
        .dispatcher
        .catch_up_push(flow.track.clone().into(), event.event, event.id)
        .await;
    let text = accepted(&flow, event.id).await.to_turn_text();
    assert!(
        text.contains(added["id"].as_str().unwrap()) && text.contains("+added prose"),
        "a located full baseline proves the added block's empty before: {text}"
    );
    mcp(&flow, "neige_report_read", json!({"blocks":[added["id"]]})).await;
    let response = flow.app.clone().oneshot(Request::patch(format!("/api/tracks/{}/report/blocks/{}", flow.track, added["id"].as_str().unwrap()))
        .header(header::COOKIE, &flow.cookie).header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({"kind":"task","markdown":null,"payload":{"key":"converted-task","kind":"terminal","command":"echo private-converted-command","ready":false,"declared_by":"user"},"ifBlockRev":added["rev"]}).to_string())).unwrap()).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let event = crate::db::RepoEventWrite::events_for_track(
        flow.fixture.repo.as_ref(),
        &flow.track,
        &["track.report_edited"],
        None,
    )
    .await
    .unwrap()
    .into_iter()
    .last()
    .unwrap();
    flow.state
        .dispatcher
        .catch_up_push(flow.track.clone().into(), event.event, event.id)
        .await;
    let text = accepted(&flow, event.id).await.to_turn_text();
    assert!(
        text.contains(added["id"].as_str().unwrap()) && !text.contains("private-converted-command"),
        "knowing prose does not certify a task payload after a kind change: {text}"
    );
    finish(flow).await;
}

#[tokio::test]
async fn report_knowledge_delta_partial_structure_and_absent_head_do_not_short_circuit() {
    let flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    flow.state
        .harness
        .get(&flow.worker)
        .unwrap()
        .pause_issuance_for_dev();
    mcp(&flow, "neige_report_read", json!({})).await;
    mcp(
        &flow,
        "neige_report_write",
        json!({"body":"# One\n\none\n\n# Two\n\ntwo\n", "message":"seed"}),
    )
    .await;
    let initial = blocks(&flow).await;
    let id = initial[0].id.clone();
    let other = initial[1].id.clone();
    user_edit(&flow, &id, "# One\n\nunseen one\n").await;
    user_edit(&flow, &other, "# Two\n\nunseen two\n").await;
    mcp(&flow, "neige_report_read", json!({"blocks":[id]})).await;
    let track = crate::ids::TrackId::from(flow.track.clone());
    let render = || {
        crate::report_read_ledger::delta::for_track(
            flow.fixture.repo.as_ref(),
            &track,
            &flow.worker,
        )
    };
    let with_head = render().await.unwrap();
    assert!(
        !with_head.contains(&id) && with_head.contains(&other),
        "partial reads certify only rendered blocks: {with_head}"
    );
    sqlx::query("DELETE FROM track_vcs_refs WHERE track_id=?")
        .bind(&flow.track)
        .execute(flow.fixture.repo.pool())
        .await
        .unwrap();
    assert_eq!(
        render().await.unwrap(),
        with_head,
        "no HEAD must not hide unread revisions"
    );
    mcp(&flow, "neige_report_read", json!({"blocks":[other]})).await;
    let separately_read = render().await.unwrap();
    assert!(
        separately_read.contains("structure_known\":false")
            && !separately_read.contains(&id)
            && !separately_read.contains(&other),
        "individual block reads do not certify whole structure: {separately_read}"
    );
    mcp(&flow, "neige_report_read", json!({})).await;
    assert!(
        render().await.unwrap().is_empty(),
        "read suggestions do not depend on history availability"
    );
    let report = read(
        &flow.app,
        &flow.cookie,
        &format!("/api/tracks/{}/report", flow.track),
    )
    .await;
    post(
        &flow.app,
        &flow.cookie,
        &format!("/api/tracks/{}/report/blocks/{other}/move", flow.track),
        "move",
        json!({"toIndex":0,"ifDocRev":report["docRev"]}),
    )
    .await;
    let text = render().await.unwrap();
    assert!(
        text.contains("doc_rev") && text.contains("structure_known\":false"),
        "structure remains separate: {text}"
    );
    assert!(
        !text.contains(&id) && !text.contains(&other) && !text.contains("block_diff"),
        "unchanged content revisions must not echo on a move: {text}"
    );
    finish(flow).await;
}

#[tokio::test]
async fn report_knowledge_delta_total_budget_keeps_every_changed_block_reference() {
    let flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    flow.state
        .harness
        .get(&flow.worker)
        .unwrap()
        .pause_issuance_for_dev();
    mcp(&flow, "neige_report_read", json!({})).await;
    let body = (0..8)
        .map(|n| format!("# Part {n}\n\nold\n\n"))
        .collect::<String>();
    mcp(
        &flow,
        "neige_report_write",
        json!({"body":body,"message":"seed"}),
    )
    .await;
    let blocks = blocks(&flow).await;
    assert_eq!(blocks.len(), 8);
    let mut last = None;
    for (n, block) in blocks.iter().enumerate() {
        let markdown = format!("# Part {n}\n\nchanged {n} {}\n", "z".repeat(1000));
        last = Some(user_edit_unaccepted(&flow, &block.id, &markdown).await);
    }
    let last = last.unwrap();
    flow.state
        .dispatcher
        .catch_up_push(flow.track.clone().into(), last.event, last.id)
        .await;
    let text = accepted(&flow, last.id).await.to_turn_text();
    for block in &blocks {
        assert!(
            text.contains(&block.id),
            "the budget must never discard a changed block's id: {}",
            block.id
        );
    }
    let diffs = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter(|value| value.get("block_diff").is_some())
        .count();
    assert!(
        diffs > 0 && diffs < blocks.len(),
        "include complete small diffs and reference the rest: {diffs}"
    );
    assert!(
        text.len() <= 8192,
        "reserve reference and framing bytes in the total budget: {}",
        text.len()
    );
    finish(flow).await;
}

#[tokio::test]
async fn report_knowledge_delta_already_read_revision_reaches_provider_without_body_or_header_echo()
{
    let mut flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    mcp(&flow, "neige_report_read", json!({})).await;
    mcp(
        &flow,
        "neige_report_write",
        json!({"body":"# Own\n\noriginal own body\n","message":"seed"}),
    )
    .await;
    let id = blocks(&flow).await[0].id.clone();
    let event = user_edit_unaccepted(&flow, &id, "# Own\n\nalready read body sentinel\n").await;
    mcp(&flow, "neige_report_read", json!({})).await;
    flow.state
        .dispatcher
        .catch_up_push(flow.track.clone().into(), event.event.clone(), event.id)
        .await;
    let knowledge = flow.trace.phase("knowledge.assertions");
    let frozen = accepted(&flow, event.id).await;
    assert!(
        frozen.to_turn_text().is_empty(),
        "an already known revision has no Report suggestion"
    );
    match (&frozen, &event.event) {
        (
            Observation::ReportEdited { body, .. },
            crate::event::Event::TrackReportEdited { body_after, .. },
        ) => assert_eq!(
            body, body_after,
            "presentation does not replace the original source content"
        ),
        _ => panic!("the actual report event retains its original observation identity"),
    }
    knowledge.complete();
    flow.end(TURN).await;
    let call = flow
        .next()
        .await
        .expect("the original source reaches its native provider");
    let provider = flow.trace.phase("provider.assertions");
    let text = call["params"]["input"].to_string();
    assert!(
        !text.contains("already read body sentinel")
            && !text.contains("original own body")
            && !text.contains(&id)
            && !text.contains("report.md"),
        "known Report body and header must not echo to the provider: {text}"
    );
    provider.complete();
    flow.accept(&call, "known-report-turn").await;
    flow.end("known-report-turn").await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    flow.close().await;
}

async fn non_report_wake_receives_delta(system: bool) {
    use calm_truth::db::RepoEventWrite as _;
    let mut flow = Flow::running().await;
    flow.state.dispatcher.abort_event_listener_for_test();
    mcp(&flow, "neige_report_read", json!({})).await;
    mcp(
        &flow,
        "neige_report_write",
        json!({"body":"# External\n\nbaseline\n\n# Own\n\nknown own sentinel\n", "message":"seed"}),
    )
    .await;
    let initial = blocks(&flow).await;
    let (external, own) = (initial[0].id.clone(), initial[1].id.clone());
    let (track, card, current) =
        crate::track_report::resolve_report_for_track(flow.fixture.repo.as_ref(), &flow.track)
            .await
            .unwrap();
    let rev = current.doc_rev;
    crate::track_report::persist_report(
        flow.fixture.repo.as_ref(),
        &flow.state.events,
        flow.state.write(),
        crate::ids::ActorId::Kernel,
        crate::event::EditAuthor::Kernel,
        track,
        card,
        current,
        crate::track_report::TrackReportPayload::new(
            "",
            "# External\n\nunseen kernel sentinel\n\n# Own\n\nknown own sentinel\n",
        ),
        rev,
        None,
    )
    .await
    .unwrap();
    if system {
        let event = crate::event::Event::TrackWakeRequested {
            track_id: flow.track.clone().into(),
            source: "objective".into(),
            key: "knowledge-non-report".into(),
            text: "ordinary system wake".into(),
        };
        let id = flow
            .fixture
            .repo
            .log_pure_event(
                crate::ids::ActorId::Kernel,
                crate::event::EventScope::Track {
                    track: flow.track.clone().into(),
                    area: flow.area.clone().into(),
                },
                None,
                &flow.state.events,
                &flow.state.card_role_cache,
                &flow.state.track_area_cache,
                event.clone(),
            )
            .await
            .unwrap();
        flow.state
            .dispatcher
            .catch_up_push(flow.track.clone().into(), event, id)
            .await;
    } else {
        flow.human().await;
    }
    let frame = flow.fixture.carrier(&flow.track).await.2;
    assert_eq!(frame.canonical_inputs.pending.len(), 1);
    assert!(
        !matches!(&frame.canonical_inputs.pending[0], PendingInput::System { observation, .. }
        if matches!(observation.observation(), Observation::ReportEdited { .. }))
    );
    flow.end(TURN).await;
    let call = flow.next().await.expect("ordinary wake reaches provider");
    let text = call["params"]["input"].to_string();
    // Finish the actual native turn before asserting so a red test leaves no live invocation.
    flow.accept(&call, "non-report-knowledge-turn").await;
    flow.end("non-report-knowledge-turn").await;
    flow.fixture
        .wait_phase(&flow.track, HarnessPhaseTag::TurnCompleted)
        .await;
    flow.close().await;
    assert!(
        text.contains(&external) && text.contains("unseen kernel sentinel"),
        "ordinary wake must include the unread Report revision: {text}"
    );
    assert!(
        !text.contains(&own) && !text.contains("known own sentinel"),
        "ordinary wake must not echo known Report content: {text}"
    );
}

#[tokio::test]
async fn report_knowledge_delta_canonical_human_wake_without_report_event() {
    non_report_wake_receives_delta(false).await;
}

#[tokio::test]
async fn report_knowledge_delta_canonical_system_wake_without_report_event() {
    non_report_wake_receives_delta(true).await;
}
