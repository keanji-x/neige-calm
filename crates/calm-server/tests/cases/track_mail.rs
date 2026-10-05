//! #2130 S1 — mail between the Tracks of one Area (`docs/architecture/2130-track-mail.md` §8): real
//! MCP calls and `neige` commands from Codex Planners whose turns run on live harnesses, the real
//! Dispatcher pushing each mail's wake.

use std::time::{Duration, Instant};

use calm_server::harness::{ClaimMode, Observation, new_track_delete_locks};
use calm_server::ids::ActorId;
use calm_server::mail::{Recipient, SendRequest};
use calm_server::model::{HarnessInputPresentation, NewArea, now_ms};
use serde_json::{Value, json};

use super::track_mail_fixture::{Planner, World, mail_and_hop, refusal};

const HANDOFF: &str = "hop 6/6 reached — hand off with neige_user_notify";

fn line(title: &str, summary: &str, mail_id: &str) -> String {
    format!("Wake from mail ({mail_id}): \"{title}\": {summary} — neige mail cat {mail_id}")
}

fn last_line(text: &str) -> &str {
    text.trim_end_matches('\n')
        .rsplit('\n')
        .next()
        .unwrap_or_default()
}

/// Row 1 (M): the send stores one row and writes the wake in one transaction; the recipient's next
/// turn input is the one line, never the body. Atomicity: an append failure stores neither.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_send_wakes_the_recipient_planner_with_one_line() {
    let w = World::new(&["Weekly review", "NVDA research"]).await;
    let (r, n) = (w.p(0), w.p(1));
    w.user_turn(r, "review this week").await;
    let body = "The guidance is below the thesis; see area/reports/review.md#b_3";
    let sent = w
        .send(
            r,
            json!({"track_id": n.track_id.as_str(), "summary": "NVDA guidance below thesis", "text": body}),
        )
        .await
        .unwrap();
    let (mail_id, hop) = mail_and_hop(&sent);
    assert_eq!(hop, "1/6");
    let row: (String, String, i64, Option<i64>, Option<String>) = sqlx::query_as(
        "SELECT from_track_id, to_track_id, hop, read_at, reply_to FROM mails WHERE id = ?1",
    )
    .bind(&mail_id)
    .fetch_one(w.repo.pool())
    .await
    .unwrap();
    assert_eq!(
        row,
        (
            r.track_id.to_string(),
            n.track_id.to_string(),
            1,
            None,
            None
        )
    );
    let wakes: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT actor, payload, scope_track FROM events WHERE kind = 'track.wake_requested'",
    )
    .fetch_all(w.repo.pool())
    .await
    .unwrap();
    assert_eq!(wakes.len(), 1);
    let expected_text =
        format!("\"Weekly review\": NVDA guidance below thesis — neige mail cat {mail_id}");
    assert_eq!(
        serde_json::from_str::<Value>(&wakes[0].0).unwrap(),
        json!({"kind": "Kernel"})
    );
    assert_eq!(
        serde_json::from_str::<Value>(&wakes[0].1).unwrap(),
        json!({"track_id": n.track_id.as_str(), "source": "mail", "key": mail_id, "text": expected_text})
    );
    assert_eq!(wakes[0].2, n.track_id.as_str());

    let turn = w.wait_turn(n, 1).await;
    assert!(
        turn.contains(&line(
            "Weekly review",
            "NVDA guidance below thesis",
            &mail_id
        )),
        "{turn}"
    );
    assert!(
        !turn.contains(body),
        "the body is never in the wake: {turn}"
    );

    // The wake stays one line: a title's line breaks become one space (the `cat` header escapes
    // them, as every text render does), and an untitled sender is named by its track id in both.
    let multiline = w.add_planner(&w.area_id, "Weekly\r\nreview").await;
    let untitled = w.add_planner(&w.area_id, "").await;
    for (sender, name, shown) in [
        (
            &multiline,
            "Weekly review".to_string(),
            "Weekly\\r\\nreview".to_string(),
        ),
        (
            &untitled,
            untitled.track_id.to_string(),
            untitled.track_id.to_string(),
        ),
    ] {
        w.user_turn(sender, "tell N").await;
        let (mail, _) = w.send_ok(sender, n, "s").await;
        let text: String = sqlx::query_scalar(
            "SELECT json_extract(payload, '$.text') FROM events \
             WHERE kind = 'track.wake_requested' AND json_extract(payload, '$.key') = ?1",
        )
        .bind(&mail)
        .fetch_one(w.repo.pool())
        .await
        .unwrap();
        assert_eq!(text, format!("\"{name}\": s — neige mail cat {mail}"));
        let (stdout, _, _) = w.neige(n, &["mail", "cat", &mail]).await;
        let header = stdout.lines().next().unwrap_or_default();
        assert!(header.ends_with(&format!("hop 1/6  {shown}")), "{header}");
    }

    // The append failure path: the event insert aborts, so the row must roll back with it.
    sqlx::query(
        "CREATE TRIGGER mail_wake_append_fails BEFORE INSERT ON events \
         WHEN NEW.kind = 'track.wake_requested' BEGIN SELECT RAISE(ABORT, 'append failed'); END",
    )
    .execute(w.repo.pool())
    .await
    .unwrap();
    let error = w
        .send(
            r,
            json!({"track_id": n.track_id.as_str(), "summary": "again", "text": "x"}),
        )
        .await
        .expect_err("the append failure fails the send");
    assert_eq!(error["code"], json!(-32603), "{error}");
    assert_eq!(w.mail_rows().await, 3, "no row without its wake");
    assert_eq!(w.wake_events().await, 3);
}

/// Cat `mail_id` as `planner` and assert the next hop the last line names.
async fn cat_next(w: &World, planner: &Planner, mail_id: &str, next: &str) {
    let (stdout, stderr, exit) = w.neige(planner, &["mail", "cat", mail_id]).await;
    assert_eq!(exit, 0, "{stderr}");
    assert_eq!(last_line(&stdout), next, "{stdout}");
}

/// Row 2 (M): §5's worked example A→B→C→A through real turns.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_hop_follows_the_worked_example() {
    let w = World::new(&["A", "B", "C"]).await;
    let (a, b, c) = (w.p(0), w.p(1), w.p(2));
    w.user_turn(a, "start the review chain").await;
    let (mut mail, hop) = w.send_ok(a, b, "m1").await;
    assert_eq!(hop, "1/6");
    w.complete(a).await;
    // Steps 2–6: each woken Planner reads the mail that woke it and mails the next Track.
    let steps = [
        (b, c, 1, 2),
        (c, a, 1, 3),
        (a, b, 2, 4),
        (b, c, 2, 5),
        (c, a, 2, 6),
    ];
    for (from, to, turn, expected) in steps {
        let text = w.wait_turn(from, turn).await;
        assert!(text.contains(&format!("neige mail cat {mail}")), "{text}");
        cat_next(&w, from, &mail, &format!("next hop {expected}/6")).await;
        let (next, hop) = w.send_ok(from, to, &format!("m{expected}")).await;
        assert_eq!(hop, format!("{expected}/6"));
        mail = next;
        w.complete(from).await;
    }
    // Step 7: A reads m6 and may not send; step 8 hands off to the user.
    w.wait_turn(a, 3).await;
    cat_next(&w, a, &mail, HANDOFF).await;
    let error = w
        .send(
            a,
            json!({"track_id": b.track_id.as_str(), "summary": "m7", "text": "x"}),
        )
        .await
        .expect_err("the seventh hop is refused");
    assert_eq!(refusal(&error, "hop_limit").0, -32409);
    let notified = w
        .call(
            a,
            "neige_user_notify",
            json!({"text": "The review loop needs you."}),
        )
        .await;
    assert!(notified.get("error").is_none(), "{notified}");
    w.complete(a).await;
    // Step 9: the user's reply restarts the chain.
    w.user_turn(a, "carry on").await;
    assert_eq!(w.send_ok(a, b, "m7").await.1, "1/6");
}

/// A NUL in the summary or text is refused as an argument (SQLite's `length()` stops at NUL, so
/// the table CHECK would otherwise fail as an internal error); nothing is stored or woken.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_send_refuses_nul_in_summary_and_text() {
    let w = World::new(&["R", "N"]).await;
    let (r, n) = (w.p(0), w.p(1));
    w.user_turn(r, "ask N").await;
    let to = n.track_id.as_str();
    for (args, field) in [
        (
            json!({"track_id": to, "summary": "a\u{0}b", "text": "t"}),
            "summary",
        ),
        (
            json!({"track_id": to, "summary": "s", "text": "a\u{0}b"}),
            "text",
        ),
        (
            json!({"track_id": to, "summary": "\u{0}b", "text": "t"}),
            "summary",
        ),
    ] {
        let error = w.send(r, args).await.expect_err(field);
        let (code, message) = refusal(&error, field);
        assert_eq!(code, -32602, "{error}");
        assert_eq!(
            message,
            format!("neige_mail_send: {field} must not contain NUL (U+0000)")
        );
    }
    assert_eq!((w.mail_rows().await, w.wake_events().await), (0, 0));
}

/// §5: the next hop is the highest of the mails read this turn, not the last one read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_hop_takes_the_max_of_mixed_reads() {
    let w = World::new(&["R", "N"]).await;
    let (r, n) = (w.p(0), w.p(1));
    let high = w.seed_mail(&r.track_id, &n.track_id, 4).await;
    let low = w.seed_mail(&r.track_id, &n.track_id, 2).await;
    w.task_turn(n).await;
    cat_next(&w, n, &high, "next hop 5/6").await;
    cat_next(&w, n, &low, "next hop 5/6").await;
    assert_eq!(w.send_ok(n, r, "answer").await.1, "5/6");
}

/// Row 3 (M): a hop-6 send is allowed, the seventh is refused with the hand-off token and stores
/// nothing. Both reading turns are wake turns.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_send_refuses_the_seventh_hop_with_the_handoff_token() {
    let w = World::new(&["A", "B"]).await;
    let (a, b) = (w.p(0), w.p(1));
    let hop4 = w.seed_mail(&a.track_id, &b.track_id, 4).await;
    w.task_turn(b).await;
    cat_next(&w, b, &hop4, "next hop 5/6").await;
    let (m5, hop) = w.send_ok(b, a, "m5").await;
    assert_eq!(hop, "5/6");
    w.complete(b).await;
    w.wait_turn(a, 1).await;
    cat_next(&w, a, &m5, "next hop 6/6").await;
    let (m6, hop) = w.send_ok(a, b, "m6").await;
    assert_eq!(hop, "6/6", "hop 6 is still allowed");
    w.complete(a).await;
    w.wait_turn(b, 2).await;
    cat_next(&w, b, &m6, HANDOFF).await;
    let (rows, wakes) = (w.mail_rows().await, w.wake_events().await);
    let error = w
        .send(
            b,
            json!({"track_id": a.track_id.as_str(), "summary": "m7", "text": "x"}),
        )
        .await
        .expect_err("refused");
    assert_eq!(
        refusal(&error, "hop_limit"),
        (-32409, format!("neige_mail_send: {HANDOFF}"))
    );
    assert_eq!((w.mail_rows().await, w.wake_events().await), (rows, wakes));
}

/// Row 4 (M): a turn the user spoke in sends hop 1 even after reading a hop-3 mail; so does a turn
/// the user steered into.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_hop_restarts_in_a_user_turn() {
    let w = World::new(&["R", "N"]).await;
    let (r, n) = (w.p(0), w.p(1));
    let first = w.seed_mail(&r.track_id, &n.track_id, 3).await;
    w.user_turn(n, "look at R's mail").await;
    cat_next(&w, n, &first, "next hop 1/6").await;
    assert_eq!(w.send_ok(n, r, "answer").await.1, "1/6");
    w.complete(n).await;

    let second = w.seed_mail(&r.track_id, &n.track_id, 3).await;
    w.task_turn(n).await;
    cat_next(&w, n, &second, "next hop 4/6").await;
    n.harness()
        .observe_for_test(
            Observation::UserMessage {
                text: "go on".into(),
            },
            None,
        )
        .await;
    let entries = n.harness().pending_entries_for_test().await;
    let view = entries
        .iter()
        .find_map(|entry| entry.user_view())
        .expect("the queued user message");
    n.harness()
        .steer_pending_entry(view.id.clone(), view.rev, ActorId::User)
        .await
        .expect("steer")
        .unwrap_or_else(|refused| panic!("the steer was refused: {refused:?}"));
    let deadline = Instant::now() + Duration::from_secs(5);
    while w.cat_json(n, &second).await["next_hop"] != json!("1/6") {
        assert!(
            Instant::now() < deadline,
            "the steer never restarted the chain"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(w.send_ok(n, r, "steered answer").await.1, "1/6");
}

/// Row 5: a reply counts the replied mail although this turn (a task completion) never read it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_reply_counts_the_replied_mail_without_reading_it() {
    let w = World::new(&["R", "N"]).await;
    let (r, n) = (w.p(0), w.p(1));
    w.user_turn(r, "ask N").await;
    let (m1, _) = w.send_ok(r, n, "question").await;
    w.complete(r).await;
    w.wait_turn(n, 1).await;
    w.complete(n).await;
    w.task_turn(n).await;
    assert_eq!(w.reply_ok(n, &m1).await.1, "2/6");
    assert_eq!(w.read_at(&m1).await, None, "the reply did not read m1");
}

/// Row 6: the read receipt is a plain write: no event, and the sender's Planner is not woken.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_read_receipt_wakes_nobody() {
    let w = World::new(&["R", "N"]).await;
    let (r, n) = (w.p(0), w.p(1));
    w.user_turn(r, "ask N").await;
    let (m1, _) = w.send_ok(r, n, "question").await;
    w.complete(r).await;
    w.wait_turn(n, 1).await;
    let watched = format!(
        "SELECT COUNT(*) FROM events WHERE scope_track = '{}' OR kind = 'track.wake_requested'",
        r.track_id
    );
    let before = w.count(&watched).await;
    assert_eq!(w.cat_json(n, &m1).await["state"], json!("read"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(w.count(&watched).await, before);
    assert_eq!(w.turns(r).len(), 1, "the sender's Planner was not woken");
    assert_eq!(r.harness().pending_len_for_test().await, 0);
}

/// Row 7 (M): the recipient's first cat stamps `read_at` and a later one keeps it; the sender's cat
/// stamps nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_cat_stamps_read_at_once_and_only_for_the_recipient() {
    let w = World::new(&["R", "N"]).await;
    let (r, n) = (w.p(0), w.p(1));
    w.user_turn(r, "ask N").await;
    let (m1, _) = w.send_ok(r, n, "question").await;
    let own = w.cat_json(r, &m1).await;
    assert_eq!(
        (&own["direction"], &own["state"]),
        (&json!("out"), &json!("unread"))
    );
    assert_eq!(w.read_at(&m1).await, None);
    let first = w.cat_json(n, &m1).await;
    assert_eq!(
        (&first["direction"], &first["state"]),
        (&json!("in"), &json!("read"))
    );
    let stamped = w.read_at(&m1).await.expect("stamped");
    tokio::time::sleep(Duration::from_millis(20)).await;
    let again = w.cat_json(n, &m1).await;
    assert_eq!(
        w.read_at(&m1).await,
        Some(stamped),
        "a later cat keeps the first read"
    );
    assert_eq!(again["read_at"], first["read_at"]);
    assert_eq!(w.cat_json(r, &m1).await["state"], json!("read"));
    assert_eq!(w.read_at(&m1).await, Some(stamped));
}

/// Row 8 (M): every recipient §6 refuses, with its code, text and kind, storing nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_send_refusals_follow_the_table() {
    let w = World::new(&["R", "N"]).await;
    let (r, n) = (w.p(0), w.p(1));
    let other_area = w
        .repo_dyn
        .area_create(NewArea {
            name: "other".into(),
            color: "#111".into(),
            sort: None,
        })
        .await
        .unwrap();
    let foreign = w
        .add_planner_without_harness(&other_area.id, "foreign")
        .await;
    let closed = w.add_track(&w.area_id, "closed").await;
    let chat = w.add_track(&w.area_id, "chat").await;
    let daily = w.add_planner_without_harness(&w.area_id, "daily").await;
    let pool = w.repo.pool();
    sqlx::query("UPDATE tracks SET closed_at = ?1 WHERE id = ?2")
        .bind(now_ms())
        .bind(closed.as_str())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query("UPDATE tracks SET purpose = 'area-chat' WHERE id = ?1")
        .bind(chat.as_str())
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO managed_track_identities (owner, identity, track_id, report_read_scope, \
         report_time_zone, tool_policy, kernel_controls_lifecycle) \
         VALUES ('test', 'daily', ?1, 'area', 'UTC', 'reports', 1)",
    )
    .bind(daily.track_id.as_str())
    .execute(pool)
    .await
    .unwrap();
    let not_mine = w.seed_mail(&n.track_id, &closed, 1).await;
    w.user_turn(r, "mail everyone").await;
    let rows = w.mail_rows().await;
    let tool = "neige_mail_send";
    let to = |track: &str| json!({"track_id": track, "summary": "s", "text": "t"});
    let cases = [
        (
            to(foreign.track_id.as_str()),
            "unknown_track",
            -32404,
            format!(
                "{tool}: no track {} in this area; list the area's tracks with neige_area_ls",
                foreign.track_id
            ),
        ),
        (
            to("no-such-track"),
            "unknown_track",
            -32404,
            format!(
                "{tool}: no track no-such-track in this area; list the area's tracks with neige_area_ls"
            ),
        ),
        (
            to(r.track_id.as_str()),
            "self",
            -32602,
            format!(
                "{tool}: track {} is this track; mail another track of the area",
                r.track_id
            ),
        ),
        (
            to(closed.as_str()),
            "closed",
            -32409,
            format!("{tool}: track {closed} is closed; hand off with neige_user_notify"),
        ),
        (
            to(chat.as_str()),
            "no_planner",
            -32409,
            format!("{tool}: track {chat} has no Planner; hand off with neige_user_notify"),
        ),
        (
            to(daily.track_id.as_str()),
            "reports_only",
            -32409,
            format!(
                "{tool}: track {} takes no mail; hand off with neige_user_notify",
                daily.track_id
            ),
        ),
        (
            json!({"mail_id": not_mine, "summary": "s", "text": "t"}),
            "unknown_mail",
            -32404,
            format!(
                "{tool}: no mail {not_mine} addressed to this track; list yours with neige mail ls"
            ),
        ),
    ];
    for (args, kind, code, message) in cases {
        let error = w.send(r, args).await.expect_err(kind);
        assert_eq!(refusal(&error, kind), (code, message));
    }
    let both =
        json!({"track_id": n.track_id.as_str(), "mail_id": not_mine, "summary": "s", "text": "t"});
    let error = w.send(r, both).await.expect_err("both");
    assert_eq!(error["code"], json!(-32602));
    // A reports-only managed caller is refused by the registry wrapper, before the handler.
    let error = w
        .send(&daily, to(n.track_id.as_str()))
        .await
        .expect_err("reports-only caller");
    assert_eq!(error["code"], json!(-32403), "{error}");
    assert_eq!((w.mail_rows().await, w.wake_events().await), (rows, 0));
    // `cat` of a mail neither to nor from the caller is refused and stamps nothing.
    let (stdout, stderr, exit) = w.neige(r, &["mail", "cat", &not_mine, "--json"]).await;
    assert_eq!((stdout.as_str(), exit), ("", 4), "{stderr}");
    assert!(
        stderr.contains(&format!("no mail {not_mine} to or from this track")),
        "{stderr}"
    );
    let error = w
        .call(r, "neige_mail_cat", json!({"mail_id": not_mine}))
        .await;
    assert_eq!(refusal(&error["error"], "unknown_mail").0, -32404);
    assert_eq!(w.read_at(&not_mine).await, None);
}

/// Row 9: a Planner with no running harness is woken when its harness next starts: the lazy
/// respawn's recovery replays the wake into its first turn.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_to_a_down_planner_is_woken_on_its_next_harness_start() {
    let w = World::new(&["R"]).await;
    let r = w.p(0);
    let n = w.add_planner_without_harness(&w.area_id, "down").await;
    w.user_turn(r, "ask the down Track").await;
    let (m1, _) = w.send_ok(r, &n, "question").await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(w.turns(&n).is_empty(), "nobody took the wake yet");
    let runtime = w
        .repo_dyn
        .session_projection_active_for_card(&n.card_id.to_string())
        .await
        .unwrap()
        .expect("the down Planner's runtime");
    let claude = calm_server::claude_planner::wiring::ClaudePlannerWiring::unconfigured_for_test(
        w.repo_dyn.clone(),
    );
    let outcome = calm_server::harness::spawn_recovered_harness(
        w.repo_dyn.clone(),
        w.events.clone(),
        w.role_cache.clone(),
        w.area_cache.clone(),
        w.daemon.clone(),
        w.daemon.thread_seals().clone(),
        &claude,
        &w.registry,
        &new_track_delete_locks(),
        runtime,
        ClaimMode::Replace,
    )
    .await
    .expect("recovery");
    let harness = outcome.installed().expect("installed");
    let expected = line("R", "question", &m1);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !w
        .turns(&n)
        .first()
        .is_some_and(|turn| turn.contains(&expected))
    {
        assert!(
            Instant::now() < deadline,
            "no first turn with the wake: {:?}",
            w.turns(&n)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    harness.shutdown().await.unwrap();
}

/// Row 10 (M): a mail's wake is a system segment, never the user's word.
#[test]
fn mail_wake_never_presents_as_user() {
    let wake = Observation::TrackWake {
        source: "mail".into(),
        key: "m1".into(),
        text: "\"R\": s — neige mail cat m1".into(),
    };
    let segments = Observation::input_segments_for(&[wake]);
    assert_eq!(segments.len(), 1);
    assert_eq!(segments[0].presentation, HarnessInputPresentation::System);
}

/// Row 13 (M): the send and the stamp re-prove the caller session in their own transaction, so a
/// superseded runtime neither sends nor marks a mail read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_send_from_a_superseded_session_is_refused() {
    let w = World::new(&["R", "N"]).await;
    let (r, n) = (w.p(0), w.p(1));
    w.user_turn(r, "ask N").await;
    let incoming = w.seed_mail(&n.track_id, &r.track_id, 1).await;
    sqlx::query("UPDATE worker_sessions SET state = 'superseded' WHERE id = ?1")
        .bind(&r.session_id)
        .execute(w.repo.pool())
        .await
        .unwrap();
    let ctx = w.app_context();
    let identity = r.identity(&w.area_id);
    let request = SendRequest {
        to: Recipient::Track(n.track_id.to_string()),
        summary: "late".into(),
        text: "from a superseded runtime".into(),
    };
    let error = calm_server::mail::send(&ctx, &identity, request)
        .await
        .expect_err("superseded");
    let error = serde_json::to_value(&error).unwrap();
    assert_eq!(
        refusal(&error, "session_inactive"),
        (
            -32403,
            "neige_mail_send: this Planner session is no longer active; nothing was sent".into()
        )
    );
    assert_eq!((w.mail_rows().await, w.wake_events().await), (1, 0));
    let error = calm_server::mail::cat(&ctx, &identity, &incoming)
        .await
        .expect_err("superseded cat");
    let error = serde_json::to_value(&error).unwrap();
    assert_eq!(
        refusal(&error, "session_inactive"),
        (
            -32403,
            "neige_mail_cat: this Planner session is no longer active".into()
        )
    );
    assert_eq!(
        w.read_at(&incoming).await,
        None,
        "no read from a superseded runtime"
    );
}

/// §2's oracle trace on Codex Planners: discovery, send, wake, read, reply, read, and `ls`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_trace_of_the_investment_case() {
    let w = World::new(&["Weekly review", "NVDA research"]).await;
    let (r, n) = (w.p(0), w.p(1));
    w.user_turn(r, "review this week").await;
    let outline =
        super::track_mail_fixture::structured(&w.call(r, "neige_area_ls", json!({})).await);
    assert!(
        outline["tracks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|track| track["id"] == json!(n.track_id.as_str())
                && track["title"] == json!("NVDA research")),
        "{outline}"
    );
    let (m1, hop) = w.send_ok(r, n, "NVDA guidance below thesis").await;
    assert_eq!(hop, "1/6");
    w.complete(r).await;
    w.wait_turn(n, 1).await;
    let (stdout, _, _) = w.neige(n, &["mail", "cat", &m1]).await;
    assert_eq!(
        stdout,
        format!(
            "{m1}  in  read  hop 1/6  Weekly review\nsummary: NVDA guidance below thesis\n\nbody\nnext hop 2/6\n"
        )
    );
    w.complete(n).await;
    w.task_turn(n).await;
    let (m2, hop) = w.reply_ok(n, &m1).await;
    assert_eq!(hop, "2/6");
    w.complete(n).await;
    w.wait_turn(r, 2).await;
    let reply = w.cat_json(r, &m2).await;
    assert_eq!(reply["reply_to"], json!(m1));
    assert_eq!(reply["next_hop"], json!("3/6"));
    let (stdout, stderr, exit) = w.neige(r, &["mail", "ls"]).await;
    assert_eq!(exit, 0, "{stderr}");
    let rows: Vec<&str> = stdout.lines().collect();
    assert_eq!(
        rows,
        [
            format!("{m2}  in  read  hop 2/6  NVDA research: reply"),
            format!("{m1}  out  read  hop 1/6  NVDA research: NVDA guidance below thesis"),
        ]
    );
}

/// `ls` pages newest first by a string cursor and stamps nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mail_ls_pages_newest_first_and_stamps_nothing() {
    let w = World::new(&["R", "N"]).await;
    let (r, n) = (w.p(0), w.p(1));
    let mut seeded = Vec::new();
    for _ in 0..51 {
        seeded.push(w.seed_mail(&n.track_id, &r.track_id, 1).await);
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    let world = &w;
    let page = |args: Value| async move {
        let response = world.call(r, "neige_mail_ls", args).await;
        super::track_mail_fixture::structured(&response)
    };
    let first = page(json!({})).await;
    let mails = first["mails"].as_array().unwrap();
    assert_eq!(mails.len(), 50);
    assert_eq!(mails[0]["mail_id"], json!(seeded[50]));
    assert_eq!(mails[0]["state"], json!("unread"));
    let cursor = first["next_cursor"]
        .as_str()
        .expect("a next page")
        .to_string();
    let second = page(json!({"cursor": cursor})).await;
    assert_eq!(second["mails"][0]["mail_id"], json!(seeded[0]));
    assert_eq!(second["next_cursor"], Value::Null);
    assert_eq!(
        w.count("SELECT COUNT(*) FROM mails WHERE read_at IS NOT NULL")
            .await,
        0,
        "listing stamps nothing"
    );
}
