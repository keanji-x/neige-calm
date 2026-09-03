//! #1252 S1 step 1 — characterization of **today's** observable write
//! semantics at every one of the three report-write decision points.
//!
//! These tests pin behaviour as it is on `origin/main`, not behaviour as it
//! ought to be. Every expected value below is a literal that was read off an
//! actual run of this suite; none of it is derived from
//! `track_report_origin`. That is deliberate: S1 step 2 threads a write-origin
//! type through these same call sites, and a test that computed its
//! expectation from that type would agree with any change it made. If one of
//! these assertions goes red, the semantics of a decision point changed —
//! confirm the change was intended before touching the assertion.
//!
//! **Covered: all 3 decision points.** It was 3 of 5 when this file was
//! written; #1300 S2 removed the other two rather than covering them, so the
//! set as it stands today is complete. Nothing keeps it that way in any strong
//! sense: the `persist_report_call_sites` CI ratchet notices a fourth writer
//! added by someone unaware of this file — it is a per-file text census, and
//! its own "KNOWN GAPS" section lists what a text scan cannot see — and this
//! header is a description, not a guard.
//!
//! | decision point | production entry driven here |
//! |---|---|
//! | `decision_sink::CardDecisionSink::commit_report_op` | the registered `calm.report.write` / `calm.report.blocks.upsert` handlers, reached through `ToolRegistry::lookup` |
//! | `routes::track_report_blocks::commit` | real axum router, `POST /api/tracks/{id}/report/blocks` |
//! | `routes::tracks::update_track_report` | real axum router, `POST /api/tracks/{id}/report` |
//!
//! What the MCP row does **not** cover: `call_tool` (in the shared
//! `mcp_track_report` fixture) does a `registry.lookup(name)` and hands the
//! handler a `ToolCallIdentity` the test constructed. So MCP token issuance,
//! the transport's token → identity binding, and the `visible_to_roles`
//! filter that decides whether a caller can see the tool at all are outside
//! the frame — nothing here would notice if they broke. What is production
//! code is the chain from the handler down: the tool handler, the decision
//! sink, the persist boundary and the role gate, driven against an
//! `AppContext` the fixture assembles. That is what makes the decision point
//! genuinely entered rather than simulated.
//!
//! Two former decision points are absent rather than uncovered:
//! `seed_template_track` and `restamp_template_report_if_placeholder`, deleted
//! by #1300 S2. Template instantiation is now structural initialization inside
//! the create transaction (`routes::tracks::prepare_template_report`), which
//! never reaches this boundary and so has no author to characterize.
//!
//! There was a sixth, `routes::track_templates::update_track_template` (#1230).
//! This file's first version listed it as "fate undecided, and it carries a
//! known attribution defect under #1291" — pinning a behaviour that was about
//! to be fixed would have made the fix harder. #1300 S1 settled the question by
//! deleting it along with the template editor it served, which also closed the
//! #1291 coupling for this path: there is no longer a write endpoint whose
//! actor gate could be missing. Six became five with S1, and three with S2.
//!
//! Per decision point, four things are observed:
//!
//! 1. **actor** — the `events.actor` column on the persisted
//!    `track.report_edited` row.
//! 2. **attribution** — `TrackReportEdited.author`, plus whether the payload
//!    carries an `author_plugin_id` key at all.
//! 3. **the consequence of `auto_promote_draft`** — not the argument, the
//!    effect: whether a track sitting in `Draft` is walked to `Planning` by
//!    the write, and what lifecycle event that produces.
//! 4. **the recorder probe** — whether the write consults the recorder gate
//!    on its `ReportWrite` leg; the section below bounds that claim.
//!
//! ## What the recorder coverage here can and cannot claim
//!
//! `persist_report_with_shadow` takes `recorder_shadow: Option<Arc<dyn
//! RecorderShadowProbe>>` and calls `probe.record(...)` inside the write
//! transaction (`track_report.rs:747` for `TrackLifecycle`, `:758` for
//! `ReportWrite`). The MCP funnel builds its probe inline
//! (`decision_sink.rs:452`, `CardDecisionSinkRecorderShadowProbe`); the
//! trait is `pub(crate)`, so an out-of-crate test cannot substitute a
//! counting double on the production assembly path.
//!
//! What *is* asserted, deterministically and from the production boundary:
//!
//! * the MCP path consults the gate **on its `ReportWrite` leg, for the
//!   `CardRole::Planner` writes these two tests drive** (the role qualifier is
//!   load-bearing — see gap 3), and a denial takes the whole transaction
//!   down with it — no
//!   `track.report_edited`, no `card.updated`, `doc_rev` still 0. Two tests,
//!   deliberately different: in
//!   `mcp_report_write_consults_the_recorder_gate_before_it_commits` the
//!   probe is *not* the only objection (see its doc comment), while
//!   `mcp_report_write_is_refused_when_the_recorder_gate_is_the_only_objection`
//!   is shaped so that it is — delete the probe and that write succeeds. The
//!   claim stops at that leg. The *other* one — the `TrackLifecycle` probe
//!   that fires only when an explicitly requested lifecycle transition
//!   applies (`track_report.rs:747`) — is reached by no call in this file:
//!   `calm.report.write` takes an optional `lifecycle` argument and none of
//!   the calls below passes it, and `calm.report.blocks.upsert` has no such
//!   argument at all. Its verdict is not observed elsewhere either, and that
//!   is an execution result rather than a caller audit: deleting the block
//!   outright leaves the whole `calm-server` package green (gap 2, item 4).
//!   The suites that do exercise that leg — `mcp_track_report.rs`'s
//!   `write_lifecycle_legal_…` and `edit_lifecycle_legal_…` — run it against
//!   an allowing gate, which is why they cannot notice;
//! * the decision *kind* is pinned, on the refusal path:
//!   `mcp_report_write_consults_the_recorder_gate_before_it_commits`
//!   asserts the refusal message contains `recorder gate denied
//!   report_write`, which is the wire form of
//!   `RecorderShadowDecisionKind::ReportWrite`. And under the
//!   probe-removal mutation that test goes red on exactly that assertion,
//!   while the sole-objection test's write starts *succeeding* — so at
//!   least one `ReportWrite` consultation is pinned too, for a Planner write.
//!   The exact number is not (gap 2), and neither is any other role's
//!   (gap 3);
//! * neither REST path is refused by the gate that refuses the MCP side —
//!   both writes commit on a track with no `worker_sessions` row at all,
//!   which is exactly the shape `decide_recorder` denies
//!   (`rest_report_writes_do_not_consult_the_recorder_gate`). Read that as
//!   the gate's *verdict* not reaching the write, not as "the `Option` is
//!   `None`": a probe that allowed unconditionally would be invisible here,
//!   which is what that test's own doc comment says. The two legs are
//!   separately covered, so pointing just one of them at a denying gate is
//!   caught.
//!
//! ### Registered gaps: what this suite stays green through
//!
//! This file's only job is to be the control group for the S1 step-2
//! refactor. A gap that is not written down here will be read as covered,
//! so all four of these are listed on purpose, and every mutation named
//! below was confirmed by applying it and watching all eight tests pass.
//!
//! **Gap 1 — where the probe sits inside the transaction is not pinned by
//! these eight tests.** Final database state cannot separate the
//! placements: a denial rolls the transaction back, so "refused before the
//! rows were written" and "rows written, then refused, then rolled back"
//! leave byte-identical observations behind, and `doc_rev == 0` cannot tell
//! them apart. Confirmed: moving the `ReportWrite` `probe.record` call to
//! after `card_update_with_crdt_tx` — still inside the transaction — leaves
//! every test here green, as does moving it above the auto-promotion
//! branch. The claim these tests do carry is the one their names make: the
//! refusal happens **before commit**, and nothing is left behind.
//!
//! This boundary is not blind to placement, though, and closing that would
//! need no new seam — only **error precedence**. Observed, not argued: give
//! `mcp_report_write_is_refused_when_the_recorder_gate_is_the_only_objection`'s
//! foreign-track session a stale `if_doc_rev` and today's code answers with
//! the recorder refusal (`-32403`, "recorder gate denied report_write"),
//! because the probe at `track_report.rs:758` runs before
//! `apply_persisted_report_op`; move the probe below that call and the same
//! request answers `-32001` "document revision conflict" instead. No
//! assertion here reads that ordering, so the placement is unpinned — but
//! unpinned by choice, not by impossibility. What the database alone really
//! cannot show is the relative order of the auto-promotion branch, the
//! `TrackLifecycle` probe and the `ReportWrite` probe on the *allow* path:
//! an allow leaves no trace of the probe behind.
//!
//! **Gap 2 — the exact probe count, and the `TrackLifecycle` leg.**
//! Production calls `record` once per write with
//! `RecorderShadowDecisionKind::ReportWrite`, plus once with
//! `TrackLifecycle` when an explicitly requested lifecycle transition fires;
//! the auto-promotion branch is not probed at all. Note what the bullets
//! above already pin, so that step 2 does not go rebuild it: at least one
//! `ReportWrite` consultation on the MCP path, and that decision kind on
//! the refusal path. What is unpinned is the *number*, and the
//! `TrackLifecycle` leg entirely. Each of the following changes was applied
//! and keeps this suite green — the list is what has been *observed*, not a
//! claim that nothing else slips through:
//!
//! 1. adding a `TrackLifecycle` `record` call to the auto-promotion branch;
//! 2. moving the `ReportWrite` `record` call to before the auto-promotion
//!    branch (a special case of gap 1);
//! 3. turning the single `ReportWrite` `record` call into several;
//! 4. **deleting the `TrackLifecycle` `record` block outright**
//!    (`track_report.rs:747`–`751`). This one is categorically worse than
//!    1–3: those reorder or repeat a gate consultation, this removes one.
//!    Confirmed by running it — the eight tests here stay green, and so does
//!    the whole `calm-server` package under
//!    `--features calm-server/codex-e2e`. What was observed is that run, not
//!    an audit of callers: nothing in the package went red when the leg was
//!    removed.
//!
//! Closing the *count* half of gap 2 needs a counting double on the
//! production assembly path, and there is no seam for one:
//! `RecorderShadowProbe` is `pub(crate)` and
//! `CardDecisionSink::commit_report_op` constructs its probe inline, so no
//! out-of-crate test can inject anything. Adding that seam is a separate
//! decision and is deliberately not taken here. Until it is, this file is a
//! control group for *whether* the `ReportWrite` leg consults the gate on
//! the paths it drives, for the kind named in the refusal, and for what a
//! denial does — and for nothing about how many times the probe is called,
//! where in the transaction it sits, whether the `TrackLifecycle` leg is
//! consulted at all, or whether some *other* caller still reaches the probe
//! (gap 3).
//!
//! **Gap 3 — the probe can be dropped conditionally, per role or per entry
//! point, and nothing here notices.** This is its own class: not position,
//! not count, not decision kind, but *which callers are still gated*.
//! Confirmed by running it — making `CardDecisionSink::commit_report_op`
//! pass `recorder_shadow: None` when `identity.role` is
//! `CardRole::Assistant`, while keeping the probe for `Planner`, leaves all
//! eight tests here green and the whole `calm-server` package green under
//! `--features calm-server/codex-e2e`. The reason it hides: both refusal
//! tests act as a Planner, and the two assistant writes (Codex and Claude)
//! only read persisted results back, which look the same whether or not an
//! allowing gate was consulted. Register it loudly because it is the shape
//! S1 step 2 is most likely to introduce by accident: threading a
//! write-origin type means rewriting exactly this funnel, which already
//! forks on `identity.role`, and a fork that quietly stops passing the
//! probe for one arm is invisible to this control group.
//!
//! **Gap 4 — the funnel's role table is invisible from this boundary.**
//! `decision_sink::report_op_attribution` maps `CardRole` to
//! `(EditAuthor, auto_promote)` and refuses `Worker` / `ReportCard`
//! outright; every call in this file arrives as `Planner` or `Assistant`, so
//! replacing that refusal arm with `(EditAuthor::Planner, true)` leaves all
//! eight tests here green — confirmed by running it, and by watching which
//! test does go red under it. S1 step 2 replaces exactly that function, and
//! the assertion that actually pins the refusal lives in another file and
//! another *target*:
//! `decision_sink::tests::report_op_attribution_refuses_worker_and_report_cards`
//! in `crates/calm-server/src/decision_sink.rs` — the crate's `--lib`
//! target, which a gate scoped to one integration binary
//! (`--test mcp_integration_suite`) never builds. Naming it here is the
//! point: a refactor that moves function and unit test together, or a gate
//! run that skips the `--lib` target, would weaken that arm with nothing in
//! this control group going red.

#![cfg(unix)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use calm_server::auth::{self, AuthConfig, AuthState, SESSION_COOKIE};
use calm_server::db::prelude::*;
use calm_server::db::sqlite::SqlxRepo;
use calm_server::event::EventBus;
use calm_server::mcp_server::tools::track_report::TOOL_REPORT_WRITE;
use calm_server::mcp_server::tools::track_report_blocks::TOOL_REPORT_BLOCKS_UPSERT;
use calm_server::model::{NewArea, NewCard, NewTrack, TrackLifecycle, TrackPatch};
use calm_server::plugin_host::{PluginHost, PluginRegistry};
use calm_server::routes;
use calm_server::state::{AppState, CodexClient, DaemonClient};
use calm_server::track_report::TrackReportPayload;
use http_body_util::BodyExt;
use serde_json::{Value, json};
use sqlx::SqlitePool;
use tower::ServiceExt;

use crate::mcp_track_report::{
    Boot, assistant_identity, boot, call_tool, planner_identity,
    seed_non_root_session_with_provider,
};
use crate::support::mcp::set_persisted_card_role;
use calm_server::mcp_server::ToolCallIdentity;
use calm_server::model::CardRole;
use calm_server::session_projection_repo::AgentProvider;
use calm_types::worker::WorkerProviderKind;

// ---------------------------------------------------------------------------
// Observation helpers — the actor and attribution expectations in this file
// are read back out of the persisted `events` table, not out of a handler's
// return value. The persisted row is what the audit log, the goldens, and
// the planner-wake decision all read.
// ---------------------------------------------------------------------------

/// `(actor, payload)` of every persisted event of `kind`, oldest first, both
/// as raw JSON. `events.actor` is stored as the serialized `ActorId` and
/// `events.payload` as the variant's content object, so comparing JSON here
/// keeps the expectations literal.
async fn persisted_events(pool: &SqlitePool, kind: &str) -> Vec<(Value, Value)> {
    let rows: Vec<(String, String)> =
        sqlx::query_as("SELECT actor, payload FROM events WHERE kind = ?1 ORDER BY id ASC")
            .bind(kind)
            .fetch_all(pool)
            .await
            .expect("read persisted events");
    rows.into_iter()
        .map(|(actor, payload)| {
            (
                serde_json::from_str(&actor).expect("events.actor is JSON"),
                serde_json::from_str(&payload).expect("events.payload is JSON"),
            )
        })
        .collect()
}

/// The single `track.report_edited` row a one-write test must have produced.
async fn only_report_edit(pool: &SqlitePool) -> (Value, Value) {
    let rows = persisted_events(pool, "track.report_edited").await;
    assert_eq!(
        rows.len(),
        1,
        "exactly one report edit expected; got {rows:#?}"
    );
    rows.into_iter().next().expect("checked length")
}

/// Attribution as it lands in the log: the `author` value, and whether the
/// payload carries an `author_plugin_id` key at all. The field is
/// `skip_serializing_if = "Option::is_none"`, so `None` is observable as the
/// key being absent — asserting on the key's absence pins exactly that.
fn assert_attribution(payload: &Value, author: &str) {
    assert_eq!(
        payload.get("author"),
        Some(&json!(author)),
        "report edit attribution; payload = {payload:#?}"
    );
    assert!(
        payload.get("author_plugin_id").is_none(),
        "author_plugin_id is absent (the field is `None` and skipped on the \
         wire) for every writer today; payload = {payload:#?}"
    );
}

// ---------------------------------------------------------------------------
// Decision point 1 — `CardDecisionSink::commit_report_op`, entered through
// the registered tool handlers (the module header says what that does and
// does not include). The fixture is the one the other MCP report suites use:
// real registry lookup, real handler, real decision sink, real recorder gate
// against real `worker_sessions` rows.
// ---------------------------------------------------------------------------

fn mcp_pool(boot: &Boot) -> SqlitePool {
    boot.repo.sqlite_pool().expect("fixture repo is sqlite")
}

async fn set_lifecycle(boot: &Boot, to: TrackLifecycle) {
    boot.repo
        .track_update(
            boot.track_id.as_str(),
            TrackPatch {
                lifecycle: Some(to),
                ..Default::default()
            },
        )
        .await
        .expect("set fixture lifecycle");
}

async fn lifecycle(boot: &Boot) -> TrackLifecycle {
    boot.repo
        .track_get(boot.track_id.as_str())
        .await
        .expect("track lookup")
        .expect("track row")
        .lifecycle
}

async fn mcp_doc_rev(boot: &Boot) -> u64 {
    calm_server::track_report_read::load_report_read_snapshot(
        boot.repo.as_ref(),
        boot.report_card_id.as_str(),
    )
    .await
    .expect("read report snapshot")
    .doc_rev
}

/// A planner agent writing the whole document over MCP: attributed to the planner,
/// actored to the planner's *session*, and it promotes a Draft track.
#[tokio::test]
async fn mcp_planner_document_write_is_planner_attributed_and_promotes_a_draft() {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Draft).await;
    let pool = mcp_pool(&boot);

    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        planner_identity(&boot),
        json!({
            "body": "# Planner wrote this\n",
            "summary": "planner summary",
            "message": "characterization write",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect("the planner may write its own track's report");

    let (actor, payload) = only_report_edit(&pool).await;
    // Observed on this suite's run: the persisted actor is the planner
    // *session*, not the planner card and not a bare `ai:planner`.
    // `PLANNER_SESSION_ID` in the shared fixture is the literal
    // `"planner-session"`.
    assert_eq!(
        actor,
        json!({"kind": "AiPlannerSession", "id": "planner-session"})
    );
    assert_attribution(&payload, "planner");

    // The consequence of auto-promotion, not the argument: the Draft track is
    // now Planning, and the transition is logged as the kernel's, with the
    // literal message the auto-promotion helper stamps.
    assert_eq!(lifecycle(&boot).await, TrackLifecycle::Planning);
    let promotions = persisted_events(&pool, "track.lifecycle_changed").await;
    assert_eq!(
        promotions.len(),
        1,
        "one auto-promotion expected; got {promotions:#?}"
    );
    let (promotion_actor, promotion) = &promotions[0];
    assert_eq!(promotion_actor, &json!({"kind": "Kernel"}));
    assert_eq!(promotion.get("from"), Some(&json!("draft")));
    assert_eq!(promotion.get("to"), Some(&json!("planning")));
    assert_eq!(
        promotion.get("agent_message"),
        Some(&json!("[auto] first planner write"))
    );
}

/// The same funnel entered by an assistant conversation: a different
/// attribution and a different auto-promotion verdict come out of it, and
/// both are decided inside the funnel from the caller's role.
#[tokio::test]
async fn mcp_assistant_block_write_is_assistant_attributed_and_leaves_a_draft_in_draft() {
    let boot = boot().await;
    set_lifecycle(&boot, TrackLifecycle::Draft).await;
    let pool = mcp_pool(&boot);

    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        assistant_identity(&boot),
        json!({
            "kind": "prose",
            "markdown": "# Assistant wrote this\n",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect("an assistant may write a prose block");

    let (actor, payload) = only_report_edit(&pool).await;
    // Observed: an assistant is actored by its *provider* session
    // (`AgentProvider::Codex` in this fixture), not by the planner-session
    // variant. `ASSISTANT_SESSION_ID` is the literal `"assistant-session"`.
    // The other provider arm is
    // `mcp_claude_assistant_block_write_is_actored_to_the_claude_session`.
    assert_eq!(
        actor,
        json!({"kind": "AiCodexSession", "id": "assistant-session"})
    );
    assert_attribution(&payload, "assistant");

    assert_eq!(
        lifecycle(&boot).await,
        TrackLifecycle::Draft,
        "an assistant write must not walk a Draft track out of Draft"
    );
    assert!(
        persisted_events(&pool, "track.lifecycle_changed")
            .await
            .is_empty(),
        "no lifecycle transition may be logged for an assistant write"
    );
}

/// A retired session's write is refused, and nothing it would have written
/// survives.
///
/// Flipping the planner session to `exited` — the state production writes when
/// a session's runtime completes (`worker_flow`, the boot reconcile in
/// `lib.rs`) — turns the same call that succeeds above into a refusal.
///
/// **The probe is not the only thing refusing here, and the substring
/// assertion below is the only thing that says which one did.** A retired
/// session is also refused by the #770 session-authority resolution
/// (`decision_gate::enforce_role_resolving_session`, `SessionNotActive`),
/// which runs on the way to the role gate. Verified, not assumed: with the
/// probe deleted from `commit_report_op` this write is still refused, and
/// the only assertion that goes red is the message one. So read this test as
/// "a retired session cannot write, and today it is the recorder gate that
/// says so first" — the load-bearing "the probe alone can refuse a write" is
/// pinned by
/// `mcp_report_write_is_refused_when_the_recorder_gate_is_the_only_objection`
/// instead.
///
/// The rollback is the timing evidence, and only for the claim in the test's
/// name: the probe runs inside the write transaction, so a refusal leaves no
/// edit, no card update, and the document at its pre-write revision. It says
/// nothing about *where* inside the transaction the probe sits — see the
/// module header's gap 1.
#[tokio::test]
async fn mcp_report_write_consults_the_recorder_gate_before_it_commits() {
    let boot = boot().await;
    let pool = mcp_pool(&boot);
    sqlx::query("UPDATE worker_sessions SET state = 'exited' WHERE id = ?1")
        .bind("planner-session")
        .execute(&pool)
        .await
        .expect("retire the planner session");

    let error = call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        planner_identity(&boot),
        json!({
            "body": "# Denied\n",
            "summary": "denied",
            "message": "characterization write",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect_err("a retired session's write is refused by the recorder gate");
    // Literal message observed on this run — it names the decision kind the
    // report-write leg of the probe passes.
    assert!(
        error.message.contains("recorder gate denied report_write"),
        "expected the recorder-gate refusal; got {error:?}"
    );

    assert!(
        persisted_events(&pool, "track.report_edited")
            .await
            .is_empty(),
        "a denied write persists no edit"
    );
    assert!(
        persisted_events(&pool, "card.updated").await.is_empty(),
        "a denied write persists no card update either — the probe ran \
         inside the transaction, before commit"
    );
    assert_eq!(
        mcp_doc_rev(&boot).await,
        0,
        "the document is untouched by a denied write"
    );
}

/// Mint a **second** track in the fixture's area, with its own Planner card and
/// its own live session bound to that card, and register the new card with
/// the role cache the way production does at boot
/// (`Repo::seed_card_role_cache`, the call `AppState::new` makes at
/// `state.rs:996`).
///
/// The session it seeds is live, card-bound, and Planner-roled — the things the
/// #770 session-authority resolution
/// (`decision_gate::enforce_role_resolving_session`) resolves a session
/// actor on — but its card lives on the *other* track, and `decide_recorder`
/// additionally requires `card.track_id == track`
/// (`crates/calm-truth/src/decision_gate.rs:324`).
async fn seed_foreign_track_planner_session(boot: &Boot, session_id: &str) {
    let track = boot
        .repo
        .track_create(NewTrack {
            template_input: None,
            area_id: boot.area_id.clone(),
            title: "foreign track".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .expect("mint the foreign track");
    let planner_card = boot
        .repo
        .card_create(NewCard {
            track_id: track.id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: Value::Null,
        })
        .await
        .expect("mint the foreign track's planner card");
    set_persisted_card_role(
        boot.repo.as_ref(),
        planner_card.id.as_str(),
        CardRole::Planner,
    )
    .await;
    seed_non_root_session_with_provider(
        boot.repo.as_ref(),
        &track.id,
        &planner_card.id,
        session_id,
        WorkerProviderKind::Codex,
    )
    .await;
    boot.repo
        .seed_card_role_cache(&boot.card_role_cache)
        .await
        .expect("re-seed the role cache from the cards table");
}

/// The recorder probe as the **sole** reason a write is refused.
///
/// The session acting here is live, in an active-authority state, and bound
/// to a `CardRole::Planner` card, so
/// `decision_gate::enforce_role_resolving_session` resolves it to
/// `ActorId::AiPlanner(that card)` and `role_gate::enforce_role` passes the two
/// card-scoped events this write emits (`card.updated`,
/// `track.report_edited`). What objects is `decide_recorder`'s
/// `card.track_id == track` clause: the session's card is on the *foreign*
/// track, while the write targets this track's report card, because the tool
/// resolves its target from `identity.card_id` — a different card on a
/// different row (`mcp_server::tools::track_report::resolve_report_for_caller`).
///
/// That "what objects" is not an argument from reading the gate, it is the
/// mutation: delete the probe from `CardDecisionSink::commit_report_op` and
/// this write commits.
///
/// Why this test exists next to the retired-session one below: there, the
/// session is also refused by the #770 authority gate, so deleting the probe
/// leaves the write refused anyway. Here, deleting the probe makes the write
/// *succeed*. That is the whole point — the assertions below are about the
/// write not happening, not about the wording of the refusal, and the error
/// message is deliberately not inspected.
#[tokio::test]
async fn mcp_report_write_is_refused_when_the_recorder_gate_is_the_only_objection() {
    let boot = boot().await;
    let pool = mcp_pool(&boot);
    const FOREIGN_SESSION_ID: &str = "foreign-track-planner-session";
    seed_foreign_track_planner_session(&boot, FOREIGN_SESSION_ID).await;

    let identity = ToolCallIdentity {
        // This track's planner card — so the tool resolves this track's report.
        card_id: boot.planner_card_id.as_str().to_string(),
        role: CardRole::Planner,
        provider: AgentProvider::Codex,
        // …but the acting session is the foreign track's.
        session_id: FOREIGN_SESSION_ID.to_string(),
        track_id: Some(boot.track_id.as_str().to_string()),
        area_id: boot.area_id.as_str().to_string(),
        thread_id: "foreign-planner-thread".to_string(),
    };

    call_tool(
        &boot,
        TOOL_REPORT_WRITE,
        identity,
        json!({
            "body": "# Denied\n",
            "summary": "denied",
            "message": "characterization write",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect_err("the recorder gate refuses a session whose card is on another track");

    assert!(
        persisted_events(&pool, "track.report_edited")
            .await
            .is_empty(),
        "a denied write persists no edit"
    );
    assert!(
        persisted_events(&pool, "card.updated").await.is_empty(),
        "a denied write persists no card update either"
    );
    assert_eq!(
        mcp_doc_rev(&boot).await,
        0,
        "the document is untouched by a denied write"
    );
}

/// The same funnel with a **Claude**-provider assistant: the persisted actor
/// is `AiClaudeSession`, not `AiCodexSession`.
///
/// `ToolCallIdentity::to_actor_id` sends every non-Planner role through
/// `registry::provider_session_actor`, which is where the provider picks the
/// actor variant. Every other identity in this file and in the shared
/// `mcp_track_report` fixture is `AgentProvider::Codex`, so without this case
/// a change that hard-coded the Codex arm would leave every other test in
/// this file green while every Claude assistant's edits silently changed
/// hands in the log.
///
/// The card is minted with `kind: "codex"` on purpose: production mints an
/// assistant conversation card that way for both providers
/// (`crates/calm-server/src/operation/planner_harness_start_adapter.rs:607`),
/// so the card's `kind` is not where the provider is recorded. What selects
/// the actor variant is `ToolCallIdentity::provider`, and *this test sets
/// that field by hand*; the `worker_sessions` row is seeded as Claude to
/// match, but it is the identity field, not the column, that
/// `to_actor_id` reads — verified by seeding the row as `Codex` and
/// watching this test stay green.
/// Production derives the identity's provider from that column
/// (`worker_sessions.provider` →
/// `calm_truth::session_projection_row::agent_provider_from_session_provider`
/// → `crates/calm-server/src/mcp_server/transport.rs:1615-1618`, which falls
/// back to `AgentProvider::Codex` when the column yields none), but
/// `call_tool` bypasses the transport entirely (see the module header), so
/// that derivation is outside this suite's frame.
#[tokio::test]
async fn mcp_claude_assistant_block_write_is_actored_to_the_claude_session() {
    let boot = boot().await;
    let pool = mcp_pool(&boot);
    const CLAUDE_SESSION_ID: &str = "assistant-claude-session";
    let card = boot
        .repo
        .card_create(NewCard {
            track_id: boot.track_id.clone(),
            title: None,
            kind: "codex".into(),
            sort: None,
            payload: json!({"schemaVersion": 1, "harness_profile": "assistant"}),
        })
        .await
        .expect("mint the claude assistant's card");
    set_persisted_card_role(boot.repo.as_ref(), card.id.as_str(), CardRole::Assistant).await;
    seed_non_root_session_with_provider(
        boot.repo.as_ref(),
        &boot.track_id,
        &card.id,
        CLAUDE_SESSION_ID,
        WorkerProviderKind::Claude,
    )
    .await;
    boot.repo
        .seed_card_role_cache(&boot.card_role_cache)
        .await
        .expect("re-seed the role cache from the cards table");

    call_tool(
        &boot,
        TOOL_REPORT_BLOCKS_UPSERT,
        ToolCallIdentity {
            card_id: card.id.as_str().to_string(),
            role: CardRole::Assistant,
            provider: AgentProvider::Claude,
            session_id: CLAUDE_SESSION_ID.to_string(),
            track_id: Some(boot.track_id.as_str().to_string()),
            area_id: boot.area_id.as_str().to_string(),
            thread_id: "claude-assistant-thread".to_string(),
        },
        json!({
            "kind": "prose",
            "markdown": "# Claude assistant wrote this\n",
            "if_doc_rev": 0
        }),
    )
    .await
    .expect("a claude assistant may write a prose block");

    let (actor, payload) = only_report_edit(&pool).await;
    assert_eq!(
        actor,
        json!({"kind": "AiClaudeSession", "id": "assistant-claude-session"})
    );
    assert_attribution(&payload, "assistant");
}

// ---------------------------------------------------------------------------
// Decision points 2 and 3 — the two REST channels, entered through the real
// router. The layering below is `main.rs`'s (and
// `rest_track_report.rs::app`'s): protected REST behind `actor_middleware`
// (innermost) and `auth::require_session` (outermost), so the session gate,
// the actor extractor and the actor pinning inside the handlers all run.
//
// The *state* is not production's. `AppState::from_parts` leaves
// `card_role_cache` and `track_area_cache` empty, where `AppState::new` seeds
// both from the database at boot (`state.rs:996`). It is harmless for what
// these two tests observe — every write here is `ActorId::User`, which
// `role_gate::enforce_role` admits without consulting either cache — and it
// would stop being harmless the moment a REST case here acted as anything
// else.
// ---------------------------------------------------------------------------

struct RestBoot {
    router: axum::Router,
    cookie: String,
    repo: Arc<SqlxRepo>,
    track_id: String,
}

impl RestBoot {
    fn pool(&self) -> &SqlitePool {
        self.repo.pool()
    }

    async fn lifecycle(&self) -> TrackLifecycle {
        self.repo
            .track_get(&self.track_id)
            .await
            .expect("track lookup")
            .expect("track row")
            .lifecycle
    }

    async fn post(&self, uri: String, body: Value) -> axum::response::Response {
        self.router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .header(header::COOKIE, self.cookie.clone())
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .expect("router responds")
    }
}

/// Fresh in-memory server with one area → one track → one track-report card,
/// plus a logged-in owner session. The track keeps the lifecycle
/// `track_create` mints, which the assertion below pins as `Draft` — that is
/// the precondition the auto-promotion observations need.
async fn rest_boot() -> RestBoot {
    let repo = Arc::new(SqlxRepo::open("sqlite::memory:").await.unwrap());
    let area = repo
        .area_create(NewArea {
            name: "report-characterization".into(),
            color: "#000".into(),
            sort: None,
        })
        .await
        .unwrap();
    let track = repo
        .track_create(NewTrack {
            template_input: None,
            area_id: area.id.clone(),
            title: "report track".into(),
            sort: None,
            cwd: String::new(),
            template_id: None,
            plugin_scope: None,
            attach_folder: false,
            theme: calm_server::routes::theme::RequestTheme::default_dark(),
        })
        .await
        .unwrap();
    assert_eq!(
        track.lifecycle,
        TrackLifecycle::Draft,
        "a freshly minted track is the Draft precondition these tests need"
    );
    repo.card_create(NewCard {
        track_id: track.id.clone(),
        kind: "track-report".into(),
        sort: Some(-1.0),
        payload: serde_json::to_value(TrackReportPayload::initial()).unwrap(),
        title: None,
    })
    .await
    .unwrap();

    let state = AppState::from_parts(
        repo.clone(),
        EventBus::new(),
        Arc::new(DaemonClient::new_stub()),
        Arc::new(PluginHost::new_full(
            Arc::new(PluginRegistry::empty()),
            repo.clone(),
            std::path::PathBuf::new(),
            std::env::temp_dir().join("calm-plugins-data-report-characterization"),
            Vec::new(),
            EventBus::new(),
            calm_server::state::WriteContext::new(
                calm_server::card_role_cache::CardRoleCache::new(),
                calm_server::track_area_cache::TrackAreaCache::new(),
            ),
        )),
        Arc::new(CodexClient::new_stub()),
        None,
        None,
    );
    let auth_state = AuthState::new(AuthConfig {
        username: Some("alice".into()),
        password: Some("hunter2".into()),
        dev_autologin: false,
        display_name: "alice".into(),
    });
    let protected_rest = routes::protected_router()
        .layer(axum::middleware::from_fn(
            calm_server::actor::actor_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            auth_state.clone(),
            auth::require_session,
        ));
    let router = axum::Router::new()
        .merge(protected_rest)
        .merge(routes::public_router())
        .with_state(state)
        .merge(auth::router().with_state(auth_state));

    let login = router
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/auth/login")
                .header("content-type", "application/json")
                .body(Body::from(
                    json!({"username": "alice", "password": "hunter2"}).to_string(),
                ))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK, "fixture login must succeed");
    let cookie = login
        .headers()
        .get(header::SET_COOKIE)
        .expect("Set-Cookie on login")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with(&format!("{SESSION_COOKIE}=")));

    let track_id = track.id.as_str().to_string();
    RestBoot {
        router,
        cookie,
        repo,
        track_id,
    }
}

/// Decision point 3 — `POST /api/tracks/{id}/report`.
#[tokio::test]
async fn rest_document_write_is_user_attributed_and_leaves_a_draft_in_draft() {
    let boot = rest_boot().await;
    let response = boot
        .post(
            format!("/api/tracks/{}/report", boot.track_id),
            json!({"summary": "human", "body": "# Human wrote this\n", "ifDocRev": 0}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let (actor, payload) = only_report_edit(boot.pool()).await;
    // Observed: the REST document channel is actored to the bare user, with
    // no session or card id riding along.
    assert_eq!(actor, json!({"kind": "User"}));
    assert_attribution(&payload, "user");

    assert_eq!(
        boot.lifecycle().await,
        TrackLifecycle::Draft,
        "a user's report write must not promote the track"
    );
    assert!(
        persisted_events(boot.pool(), "track.lifecycle_changed")
            .await
            .is_empty(),
        "no lifecycle transition may be logged for a REST document write"
    );
}

/// Decision point 2 — `POST /api/tracks/{id}/report/blocks`.
#[tokio::test]
async fn rest_block_write_is_user_attributed_and_leaves_a_draft_in_draft() {
    let boot = rest_boot().await;
    let response = boot
        .post(
            format!("/api/tracks/{}/report/blocks", boot.track_id),
            json!({"kind": "prose", "markdown": "# Human block\n", "ifDocRev": 0}),
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);

    let (actor, payload) = only_report_edit(boot.pool()).await;
    assert_eq!(actor, json!({"kind": "User"}));
    assert_attribution(&payload, "user");

    assert_eq!(
        boot.lifecycle().await,
        TrackLifecycle::Draft,
        "a user's block write must not promote the track"
    );
    assert!(
        persisted_events(boot.pool(), "track.lifecycle_changed")
            .await
            .is_empty(),
        "no lifecycle transition may be logged for a REST block write"
    );
}

/// Neither REST channel consults a recorder gate that would refuse them.
///
/// The evidence is the track these writes land on: it has no
/// `worker_sessions` row at all, which is precisely the shape the gate
/// refuses on the MCP side (`decide_recorder` denies a principal with no
/// session row, and the probe additionally refuses outright when there is no
/// agent principal to gate on). Both REST writes nevertheless commit, so
/// neither leg ran a probe that consulted that gate. A probe that allowed
/// unconditionally would be invisible here — the claim is about the gate's
/// verdict reaching the write, not about the `Option` being `None`.
///
/// The two legs are covered independently, not as a pair: pointing only the
/// document leg (`routes::tracks::update_track_report`) at a denying gate
/// turns this test and
/// `rest_document_write_is_user_attributed_and_leaves_a_draft_in_draft`
/// red, and pointing only the block leg
/// (`routes::track_report_blocks::commit`) at one turns this test and
/// `rest_block_write_is_user_attributed_and_leaves_a_draft_in_draft` red.
/// That matters because the two legs reach the write boundary by different
/// doors: the document leg goes through `persist_report`, the wrapper that
/// hard-codes `recorder_shadow: None`, while the block leg calls
/// `persist_report_with_shadow` and chooses the argument itself. A single test
/// covering "one of them" would leave whichever door it did not use unpinned.
///
/// #1300 S2 — this sentence used to say the wrapper's `None` was "shared with
/// the template seed/restamp writers". It is not shared with anything in
/// production any more: `seed_template_track` and
/// `restamp_template_report_if_placeholder` are deleted, leaving
/// `routes::tracks::update_track_report` as `persist_report`'s only production
/// caller as of today (the `persist_report_call_sites` census is what would
/// notice a second one being added inadvertently; it is a text census, not a
/// proof).
/// The reason to cover the leg independently survives the sharing, because it
/// was never about who else passed the argument — it is about this leg not
/// choosing it.
///
/// This is a statement about *whether* the gate is consulted, not about an
/// exact invocation count; see the module header's registered gaps.
#[tokio::test]
async fn rest_report_writes_do_not_consult_the_recorder_gate() {
    let boot = rest_boot().await;
    let (sessions,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM worker_sessions")
        .fetch_one(boot.pool())
        .await
        .expect("count sessions");
    assert_eq!(
        sessions, 0,
        "the REST fixture deliberately has no agent session to gate on"
    );

    let document = boot
        .post(
            format!("/api/tracks/{}/report", boot.track_id),
            json!({"summary": "human", "body": "# One\n", "ifDocRev": 0}),
        )
        .await;
    assert_eq!(document.status(), StatusCode::OK);
    let block = boot
        .post(
            format!("/api/tracks/{}/report/blocks", boot.track_id),
            json!({"kind": "prose", "markdown": "# Two\n", "ifDocRev": 1}),
        )
        .await;
    assert_eq!(
        block.status(),
        StatusCode::OK,
        "block write body = {:?}",
        block.into_body().collect().await.unwrap().to_bytes()
    );

    assert_eq!(
        persisted_events(boot.pool(), "track.report_edited")
            .await
            .len(),
        2,
        "both REST writes committed on a track with no agent session"
    );
}
