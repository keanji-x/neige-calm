//! #1318 §1 — the report write boundary, closed by the compiler.
//!
//! ## What this module is
//!
//! [`persist`] is the one function that performs a report **edit**: it applies
//! a [`ReportDocOp`] to the CRDT and emits the `CardUpdated` + `…Edited` pair.
//! It is not the only code in this file that writes the report card's row:
//! since #1252 S2 the create-time paths write it too, through
//! [`structural_init_report_tx`], and no claim here should be read as saying
//! otherwise. What changed with that slice is *where* those writes are, not
//! that they stopped — see "What is still not closed", item 4.
//!
//! It is **private to this module**, and that closes the caller set in two
//! steps, which are worth separating because only the first one is `rustc`'s:
//!
//! 1. **`rustc`** limits callers to `mod write` *and its descendants* — Rust
//!    privacy is per module subtree, not per file. A call added in any other
//!    module does not compile (`error[E0603]: function `persist` is private`,
//!    reproduced on this branch). This half is a proof and it is the one that
//!    carries the slice.
//! 2. **`scripts/ci/ratchets/report_write_boundary.sh`** tries to keep this
//!    module from acquiring descendants — no `mod`, no `#[path]`, no
//!    `include!`, no macro outside a one-name allowlist, no `use … as` alias,
//!    no `r#`. It is a **drift detector, not a proof**, and the honest reading
//!    of "one file" stops here.
//!
//!    That wording is the third attempt, and the first two were both wrong in
//!    the same direction — they claimed the gate *closed* the question. Review
//!    channels broke each of them with constructions that compile:
//!
//!    * a macro **defined in another file**, invoked here on one line,
//!      expanding to `pub(crate) mod smuggled { … super::persist(.., Kernel, ..) }`
//!      — the blocklist looked for `macro_rules!` declared here and for a
//!      literal `mod`, and this has neither;
//!    * `use std::include as format;` — renaming a builtin macro *onto* the
//!      allowlist, the same `use … as` shape that walked past #1300's census;
//!    * a multi-line `#[doc = r#"…"#]` whose body reads as an attribute, and a
//!      rustfmt-wrapped multi-line `#[cfg(all(…))]` that split the attribute
//!      block in two.
//!
//!    Each is now rejected, and that is exactly the point: **the list grew
//!    every round.** An attribute or derive proc-macro still expands to
//!    whatever it likes with no `ident!` for any regex to find. So the claim
//!    this module makes is the one #1300's census made about itself, and no
//!    more: it catches somebody adding a writer *without knowing this boundary
//!    exists*. It does not catch somebody working around it.
//!
//!    What is closed is step 1, and step 1 alone: the caller set is `mod write`
//!    and its descendants, by `rustc`. Read every "one file" in this repository
//!    as "one file, as far as a text scan can tell".
//!
//! Step 1 is the part #1300 S2's text census could only approximate, and it is
//! the part that carries this slice. Step 2 is a text rule over one file
//! instead of the repository — smaller than the census, and the same kind of
//! thing: a review aid, with declared gaps.
//!
//! That census (`scripts/ci/ratchets/persist_report_call_sites.sh`) scanned the
//! whole repository for two identifiers and shipped a `KNOWN GAPS` section that
//! four review rounds kept adding to — line-broken calls, `use ... as` aliases,
//! macro paths, `#[path]` modules outside `src`, `include!`-computed paths,
//! non-`.rs` compiled sources, equal-count substitutions. None were regex bugs.
//! A text scan cannot decide what `rustc` compiles, so it could never be
//! finished. The fix is not a better scan; it is to stop needing one, by making
//! the caller set a *module*.
//!
//! ## The entry points
//!
//! Everything outside this module's subtree goes through one of these, and each
//! is named for the decision point it serves:
//!
//! | entry | production caller | attribution |
//! |---|---|---|
//! | [`rest_user_replace`] | `routes::tracks::update_track_report` | `User`, fixed |
//! | [`rest_user_block_op`] | `routes::track_report_blocks::commit` | `User`, fixed |
//! | [`agent_report_op`] | `decision_sink::CardDecisionSink::commit_report_op` | caller-supplied; that caller derives it from `identity.role` |
//! | [`structural_init_report_tx`] | `routes::tracks::create_track_structure` | **none — the signature has no way to name one** |
//!
//! The fourth row is a different kind of entry from the first three and the
//! table would mislead without this sentence: it does not reach [`persist`],
//! and it is not an edit. It is #1252 S2's **structural door** — track creation
//! laying a forked or templated report onto the card it just INSERTed, inside
//! the create transaction. It shares exactly the row write and the task
//! projection with [`persist`] (the private
//! [`write_report_row_and_project_tx`], where the order between those two
//! statements is the contract) and nothing else. Its six absences —
//! no author, no actor, no `EventBus`, no CAS input, no lifecycle /
//! auto-promote, no recorder probe — are documented one by one on the function,
//! and `tests/cases/fork_guard_exemption_invariant.rs` parses this file to keep
//! them absent.
//!
//! The two REST entries do not *take* an `ActorId` or an `EditAuthor`. Both
//! handlers used to pass `User` / `User` as arguments under a comment saying
//! they always would; now the signature says it.
//!
//! That narrows those two *entries* and nothing else. It does **not** mean a
//! REST handler can no longer write as `Spec`: it can call [`agent_report_op`]
//! instead, which is `pub(crate)` and takes an arbitrary `ActorId` /
//! `EditAuthor` / `auto_promote_draft` / probe. Item 1 of "What is still not
//! closed" is that door; it is open by construction and nothing here should be
//! read as implying otherwise. It keeps those parameters because the role →
//! attribution decision genuinely lives at the MCP funnel
//! (`decision_sink::report_op_attribution`, exhaustive on `CardRole`), and
//! moving it here would relocate it, not close it.
//!
//! ## What "three sites, each honest" is carried by, after this slice
//!
//! Read the claim precisely, because the boundary closes one half of it and
//! not the other, and the old census's mistake was letting the two blur:
//!
//! * *only three* — **closed for the boundary's own signature set**: three is
//!   the number of `pub(crate)` doors that reach [`persist`], and a fourth such
//!   door has to be cut in this file because nothing else can reach it.
//!   (#1252 S2 added a fourth `pub(crate)` entry,
//!   [`structural_init_report_tx`], which is why "three doors" and "three
//!   `pub(crate)` fns" are no longer the same count: that one does not reach
//!   [`persist`] and performs no edit.) It is emphatically *not*
//!   "only three (actor, author, auto-promote, probe) tuples ever reach the
//!   writer": [`agent_report_op`] takes all four from its caller, so any
//!   sibling module can compose a new one without touching this file. Item 1
//!   below is that hole, stated in full.
//! * *each honest* — `tests/cases/report_write_characterization.rs` drives all
//!   three decision points through the real router / tool registry and asserts
//!   the persisted `events.actor` and `TrackReportEdited.author`. Still a test,
//!   still the carrier for this half.
//!
//! ## What is still not closed, stated plainly
//!
//! 1. **Who may call these entries is not bounded, and through
//!    [`agent_report_op`] neither is what they may say.** All three are
//!    `pub(crate)`, so any sibling can call any of them — exactly as any
//!    sibling could call the old `pub(crate) persist_report_with_shadow`. Two
//!    consequences, the second sharp:
//!
//!    * A new caller of `rest_user_replace` / `rest_user_block_op` produces the
//!      same `User`-attributed, un-auto-promoting, un-gated edit the REST
//!      handlers do — bounded by the signature, which cannot say anything else.
//!    * A new caller of `agent_report_op` is bounded by nothing. It hands over
//!      `ActorId`, `EditAuthor`, `auto_promote_draft` and the probe, so a
//!      sibling can compose a tuple no production path uses today — including
//!      `EditAuthor::Kernel`, or a `RecorderShadowProbe` that always allows —
//!      **without editing this file**. Nothing here stops that and no wording
//!      may imply it does; what stops it is review of a new call site, plus
//!      `tests/cases/report_write_characterization.rs` for the sites that
//!      exist.
//!
//!    The narrower thing — one named module per entry — is not expressible in
//!    Rust: `pub(in path)` accepts only ancestor modules, and an unrelated
//!    module is not one.
//! 2. **A witness argument would not fix (1), so there isn't one.** The obvious
//!    move is to make the REST entries demand a token minted by
//!    `routes::track_report_blocks::require_rest_user_actor`. It would prove
//!    nothing: `Actor` is `pub struct Actor(pub String)` (`actor.rs:57`), so
//!    any module can build `Actor("user".into())` and mint the token. That is a
//!    marker, not a guard, and this file is not going to ship one.
//! 3. **Editing this module.** The boundary makes a new *door* land in this
//!    subtree rather than anywhere; it does not make landing here impossible.
//!    `scripts/ci/ratchets/report_write_boundary.sh` pins the shapes that would
//!    quietly widen it — [`persist`] gaining a `pub` or a `#[cfg]`, a `mod` /
//!    `#[path]` / `include!` / `macro_rules!` / `impl` appearing here (each of
//!    which extends the caller set or the entry set to something the rules
//!    cannot read), a `pub use` re-export, and the exported entry set changing.
//!    That gate is text over one file, so it inherits the usual limits — it is
//!    the *residue* after `rustc` did the load-bearing part, not a repeat of
//!    the census.
//! 4. **Other code writing the report card's row directly.** The class is open
//!    and it is not only about hand-written SQL: a bare `UPDATE cards SET
//!    payload = ..., body_crdt = ...` in any `write_with_*_typed` closure
//!    rewrites the row without coming near this module, no module boundary
//!    reaches that, and #1252 §3 P2 records why it has no local solution. So
//!    "[`persist`] is the only thing that touches the row" would still be false
//!    and is not claimed anywhere here.
//!
//!    What #1252 S2 changed is the one member this list has ever been able to
//!    name. Until that slice the create paths reached
//!    `card_update_with_crdt_tx` through
//!    `routes::tracks::persist_initial_report_and_project_tasks_tx`, so this
//!    item read "declared, not eliminated". That function is gone: the create
//!    paths enter through [`structural_init_report_tx`], and
//!    `card_update_with_crdt_tx` now has **one** production call file — this
//!    one. Read that as the honest thing it is: every production writer of the
//!    row that anyone has found is now inside this boundary, and a *new* one
//!    could still be written outside it tomorrow with nothing here noticing.
//!    Eliminated for the known members, not closed as a class.
//! 5. **Which `EditAuthor` [`agent_report_op`] is handed.** Closed for the two
//!    REST entries by their signatures; for the MCP entry it remains the
//!    characterization suite's job.
//! 6. **That the `Track` and the `Card` in a [`ReportEditTarget`] belong
//!    together.** The constructor compares `report_card.track_id` to
//!    `track.id`, which catches an accidental pairing and nothing more:
//!    `Card::track_id` is a `pub` field, so a caller can clone the real card
//!    and overwrite it. That is the same forgeable-marker shape item 2 rejects,
//!    and it is labelled as a drift catch where it lives rather than dressed up
//!    here. Closing it for real means checking the row inside the write
//!    transaction — scoping the `UPDATE` by track, or reading the owner back
//!    before the write. Neither is in this slice, and the reason is scope, not
//!    difficulty: `card_update_with_crdt_tx` is shared truth-layer code with
//!    callers outside this module, so narrowing it is its own change with its
//!    own caller sweep. `current_payload` has no comparison at all.
//!
//! ## The test-only escape hatch
//!
//! [`persist_report`] is `pub` under `cfg(any(test, feature = "fixtures"))`
//! only — ten integration-test files plus two in-crate `#[cfg(test)]` modules
//! (`report_backlinks`, `track_report_read`) drive the boundary directly with a
//! hand-picked `EditAuthor`, which is the point of a characterization test and
//! must stay possible. `fixtures` is enabled for test builds through the
//! `[dev-dependencies]` self-loop in `crates/calm-server/Cargo.toml`, so a
//! plain `cargo build -p calm-server` compiles zero bytes of this entry.
//!
//! What that is **not**: a compiler-enforced property. `fixtures` is an
//! ordinary additive feature — `cargo build --release -p calm-server --features
//! fixtures` compiles this entry into the server binary, and the `replay` bin
//! declares `required-features = ["fixtures"]`, so there are real builds in
//! this repo with the feature on. The thing that keeps it out of production is
//! the convention stated in the feature's own doc in `Cargo.toml` ("Production
//! builds ... MUST NOT enable this feature"), the same convention that already
//! holds back `AppState::raw_repo()` — a strictly larger hole, since it hands
//! out the repo and bypasses the write gate entirely. This entry does not make
//! that convention weaker; it also does not get to claim a guarantee the
//! convention cannot give.

use super::*;

// ---------------------------------------------------------------------------
// Entry points — the complete set of ways to reach `persist` from outside,
// plus the structural door, which reaches the row write without reaching
// `persist` at all
// ---------------------------------------------------------------------------

/// The arguments of [`structural_init_report_tx`], as one value.
///
/// A struct rather than six positional parameters for the ordinary reason (four
/// of the six are borrowed slices or strings and two of those are `&str`), and
/// for one that is specific to this door: the absences documented on
/// [`structural_init_report_tx`] are only readable if the whole argument set is
/// readable in one place. This is that place, and it holds exactly the report
/// content and the two ids the row write and the projection need — nothing that
/// names a writer, an authority or a prior revision.
///
/// The `doc` arrives **already authoritative**, and this door applies no op to
/// it. Both producers (`routes::tracks::prepare_fork_report` via
/// `ReportDoc::from_blocks_exact`, and
/// `routes::tracks::prepare_initial_report_payload` via
/// `ReportDoc::from_payload`) settle the document before handing it over —
/// `from_blocks_exact` in particular bypasses `write_blocks_layout` precisely so
/// each block keeps the `(id, rev)` it had in the source track. That is why this
/// door does not route through `apply_report_op`: `apply_persisted_report_op`
/// unconditionally advances `doc_rev` and re-stamps every block's `rev`, which
/// would move a fork's `docRev` from 0 to 1 and break the on-the-wire fork
/// contract.
///
/// The borrow is `&mut` only because serializing an `automerge::AutoCommit`
/// takes `&mut self` (`ReportDoc::to_bytes` → `AutoCommit::save`), which is the
/// same reason `persist` holds its own doc mutably. It is not a licence for this
/// door to edit the document, and it does not.
pub(crate) struct InitialReportTarget<'a> {
    /// The report card row this write lands on. It was INSERTed earlier in the
    /// same transaction, which is why there is no CAS input anywhere here.
    pub report_card_id: &'a str,
    /// The track that owns the card, used for the task projection.
    pub track_id: &'a str,
    /// The projected payload — the JSON read cache mirroring `doc`.
    pub payload: &'a TrackReportPayload,
    /// The authoritative CRDT document. `&mut` for serialization only — see
    /// [`structural_init_report_tx`].
    pub doc: &'a mut ReportDoc,
    /// The task declarations projected out of the report's blocks.
    pub declarations: &'a [calm_types::report_blocks::tasks::TaskDeclaration],
    /// Per-block local diagnostics, aligned with the blocks the declarations
    /// came from.
    pub diagnostics: &'a [Vec<calm_types::report_blocks::tasks::Diagnostic>],
}

/// `POST /api/tracks/{id}/report` — the user's wholesale report replace.
///
/// Attribution is not a parameter. `routes::tracks::update_track_report` gates
/// the route to `X-Calm-Actor: user` and its OpenAPI doc claims every other
/// actor is 403; this signature is what makes the second half of that claim
/// ("and therefore only a User edit can be recorded") true by construction
/// rather than true because a downstream argument happened to be hardcoded.
pub(crate) async fn rest_user_replace(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    target: ReportEditTarget,
    next: TrackReportPayload,
    if_doc_rev: u64,
) -> Result<Card, CalmError> {
    let (updated, _block) = persist(
        repo,
        events,
        write,
        ActorId::User,
        EditAuthor::User,
        target,
        ReportDocOp::Replace {
            summary: Some(next.summary),
            body: next.body,
            if_doc_rev,
        },
        None,
        None,
        false,
        None,
    )
    .await?;
    Ok(updated)
}

/// `POST|PATCH|DELETE /api/tracks/{id}/report/blocks*` — the user's typed
/// block-channel edits.
///
/// Same fixed attribution as [`rest_user_replace`], and the same reason. The
/// `auto_promote_draft = false` is also fixed here: the REST block endpoints
/// have always passed `false`, so "a Draft track that has a report" is a
/// long-standing legal state rather than something a caller may opt into.
pub(crate) async fn rest_user_block_op(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    target: ReportEditTarget,
    op: ReportDocOp,
) -> Result<(Card, Option<BlockOpOutcome>), CalmError> {
    persist(
        repo,
        events,
        write,
        ActorId::User,
        EditAuthor::User,
        target,
        op,
        None,
        None,
        false,
        None,
    )
    .await
}

/// The agent-MCP funnel — `calm.report.write` / `calm.report.edit` /
/// `calm.report.blocks.*`, reached through
/// `decision_sink::CardDecisionSink::commit_report_op`.
///
/// Unlike the two REST entries this one takes its attribution, its
/// auto-promote verdict and its recorder-shadow probe. Only the first two come
/// from the role: `decision_sink::report_op_attribution` maps
/// `ToolCallIdentity::role` to `(EditAuthor, auto_promote_draft)`, exhaustively
/// on `CardRole` so a new role must state its own answer. The probe is
/// assembled from the full principal and the target track
/// (`CardDecisionSinkRecorderShadowProbe { principal, track_id }`), and what the
/// gate reads of it is the `session_id` — not the role. Pulling either decision
/// in here would move it without narrowing anything, and would put a `CardRole`
/// dependency on the persist boundary.
///
/// **This entry is `pub(crate)` and validates none of the three.** Any sibling
/// module can call it with any `ActorId` / `EditAuthor` / `auto_promote_draft`
/// and a probe that always allows. That is item 1 of the module header's
/// "What is still not closed" — the boundary bounds the set of doors, not what
/// a caller says when it walks through this one.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn agent_report_op(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    actor: ActorId,
    author: EditAuthor,
    target: ReportEditTarget,
    op: ReportDocOp,
    agent_message: Option<String>,
    lifecycle: Option<TrackLifecycle>,
    auto_promote_draft: bool,
    recorder_shadow: Arc<dyn RecorderShadowProbe>,
) -> Result<(Card, Option<BlockOpOutcome>), CalmError> {
    persist(
        repo,
        events,
        write,
        actor,
        author,
        target,
        op,
        agent_message,
        lifecycle,
        auto_promote_draft,
        Some(recorder_shadow),
    )
    .await
}

/// #1252 S2 — the **structural door**: track creation laying a forked or
/// templated report onto the report card it just INSERTed, inside the create
/// transaction.
///
/// This is not a report *edit* and it is not a fourth way to reach [`persist`]:
/// it never calls it. What it shares with [`persist`] is the last two
/// statements — the row write and the task projection, in that order — through
/// the private [`write_report_row_and_project_tx`]. Everything an edit needs
/// and a structural initialization does not, this door does not have. Before
/// this slice the same two statements lived in
/// `routes::tracks::persist_initial_report_and_project_tasks_tx`, which is why
/// this module's header used to list the create paths as a second writer of the
/// row; it no longer is one.
///
/// # The six absences, and why each is load-bearing
///
/// Read these as the signature's content, not as missing features. Each is a
/// thing that *cannot be said here*, and the reason it cannot is different in
/// every case:
///
/// * **No `author: EditAuthor`.** #1115. `guard_task_declarations` is
///   unreachable from this function because there is nothing to pass it: no
///   parameter of this door carries an author, and the create path deliberately
///   derives none (`routes::tracks::create_track_structure` says so at the
///   closure it builds). The fork's own belt over that decision —
///   `routes::tracks::fork_guard::guard_forked_blocks` — deliberately stays
///   **upstream**, in `prepare_fork_report`, next to the normalization it belts.
///   Hanging it here would give `TrackInit::Template` a guard it has never had
///   and separate the belt from what it belts. An `Option<EditAuthor>` is the
///   same hole with a nullable type; see `track_report_origin`'s header.
/// * **No `&EventBus`, and no event in the return type.** Q12. This door
///   **cannot emit** — not "emits nothing today". The create transaction owns
///   the report card's only event, one `Event::CardAdded` attributed to
///   `actor.to_actor_id()`, and it is emitted by the create closure that calls
///   this. So a fork or a template instantiation produces no
///   `track.report_edited` and no `card.updated`, because there is no bus here
///   to put one on.
/// * **No `ActorId`.** The one event that exists is the create closure's, and
///   the closure holds the actor. Threading one in here would give this door an
///   attribution it has nothing to attribute.
/// * **No `if_doc_rev` / `expected_rev`.** The target row was INSERTed by
///   `card_create_with_id_tx` earlier in *this same transaction*, so there is no
///   prior version for a compare-and-swap to be against. A CAS input here could
///   only ever compare a value with the one this transaction just wrote.
/// * **No `TrackLifecycle`, and no `auto_promote_draft`.** A track being created
///   has no lifecycle to transition and no draft state to promote out of; both
///   of [`persist`]'s lifecycle legs are about a track that already exists.
/// * **No `RecorderShadowProbe`.** The recorder gate decides whether an *agent
///   principal* may write. Track creation has no agent principal —
///   `create_track_structure` runs on a REST request whose only identity is the
///   `Actor` extractor's — so there is no session for a probe to read.
///
/// None of the six appears in this signature, and none can be added to it
/// quietly: `tests/cases/fork_guard_exemption_invariant.rs` pins both
/// parameters, all six fields of [`InitialReportTarget`] and the return type
/// **by name and by written type**, so acquiring any of them is a decision
/// somebody has to record in that file. That is a gate against drift, not a
/// proof about the language — the earlier wording here ("none of the six is
/// expressible by a caller") claimed the latter, and a review channel broke it
/// twice with newtypes that kept every pinned name. What is true without
/// qualification is the narrower thing: as written today this door takes a
/// transaction and report content, so there is no author to hand
/// `guard_task_declarations` and no `track_report_origin::WritePolicy` field to
/// read — three of that struct's four fields are consumed here by *not
/// existing*, and the fourth (`actor`) is consumed by the create closure,
/// outside this boundary.
pub(crate) async fn structural_init_report_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    target: InitialReportTarget<'_>,
) -> Result<(Card, TaskProjectionOutcome), CalmError> {
    write_report_row_and_project_tx(
        tx,
        target.report_card_id,
        target.track_id,
        target.payload,
        target.doc,
        target.declarations,
        target.diagnostics,
    )
    .await
}

/// Test-only direct access to the boundary, with the pre-#1318 signature.
///
/// Not an entry point in the sense above: it exists so characterization and
/// integration tests can write as any `EditAuthor` and observe what comes out,
/// which is exactly the ability production code must not have. Gated to
/// `cfg(any(test, feature = "fixtures"))`, which keeps it out of a default
/// build and **not** out of every release build — `fixtures` is an ordinary
/// additive feature and the `replay` bin requires it. See this module's
/// header; the guarantee is a convention in `Cargo.toml`, not the compiler.
#[cfg(any(test, feature = "fixtures"))]
#[allow(clippy::too_many_arguments)]
pub async fn persist_report(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    actor: ActorId,
    author: EditAuthor,
    track: Track,
    report_card: Card,
    current_payload: TrackReportPayload,
    next: TrackReportPayload,
    if_doc_rev: u64,
    agent_message: Option<String>,
    lifecycle: Option<TrackLifecycle>,
    auto_promote_draft: bool,
) -> Result<Card, CalmError> {
    let (updated, _block) = persist(
        repo,
        events,
        write,
        actor,
        author,
        ReportEditTarget {
            track,
            report_card,
            current_payload,
        },
        ReportDocOp::Replace {
            summary: Some(next.summary),
            body: next.body,
            if_doc_rev,
        },
        agent_message,
        lifecycle,
        auto_promote_draft,
        None,
    )
    .await?;
    Ok(updated)
}

// ---------------------------------------------------------------------------
// The boundary itself — private to this module, and that is the whole point
// ---------------------------------------------------------------------------

/// Apply one [`ReportDocOp`] to the report card's CRDT, write the row,
/// and emit `Event::CardUpdated` + `Event::TrackReportEdited` from the same
/// transaction. Returns the updated `Card` (callers build their wire response
/// from it) plus the op's [`BlockOpOutcome`] where it has one.
///
/// **Private, and that is this function's most important property.** Every
/// report *edit* goes through here — the spec-MCP tools
/// (`calm.report.write` / `calm.report.edit` / `calm.report.blocks.*`) and both
/// REST legs alike — so the CRDT-write + dual-event invariant holds uniformly:
/// every **successful** call → one `CardUpdated` + one `TrackReportEdited`. A
/// call that fails — a denied recorder probe, an `if_rev` / `if_doc_rev`
/// conflict, a rejected fence, a database error — aborts the transaction and
/// emits neither.
///
/// "Every edit", not "every write to the row": the create-time paths lay a
/// template or forked report down without coming near this function — since
/// #1252 S2 they do it through this module's own
/// [`structural_init_report_tx`], which shares the row write with this function
/// and nothing else (module header, "What is still not closed", item 4).
/// Before #1318 §1 even the narrower claim was about a `pub(crate)`
/// function anybody in the crate could call; now it is about a function only
/// this module's subtree can reach, with the entry points above as the list of
/// doors that exist today — a list this module maintains, not one the compiler
/// enumerates.
///
/// Issue #247 PR1 — materializes the opaque CRDT blob alongside the
/// legacy `payload` JSON. The CRDT is authoritative; the JSON column
/// is a read-cache the existing v1 REST / WS read paths and the
/// frontend continue to consume.
///
/// Issue #247 PR2 — every successful call also emits a structured
/// `Event::TrackReportEdited` carrying `(summary_before, summary_after,
/// body_before, body_after, author, edit_id)` so PR4's UI can render an
/// edit timeline and PR5's spec agent can wake on user-authored edits. A
/// failed call emits nothing: the transaction carries both events.
///
/// Issue #247 PR3 — `author` became a parameter (was hard-coded `Spec`), and
/// #1318 §1 narrowed who may supply it: only [`agent_report_op`] passes one
/// through from its caller, and it comes from `report_op_attribution`'s
/// exhaustive match on `CardRole`. The two REST entries fix `EditAuthor::User`
/// in their own bodies. The `EditAuthor::Kernel` arm has no production caller
/// — #1300 removed the last one — and is reserved for future server-internal
/// rewrites.
///
/// In-tx sequence:
///
///   1. Read the current `body_crdt`. NULL = first post-PR1 write on
///      this row (legacy seed / pre-#247 mint); seed a fresh doc from
///      `current_payload`. Non-NULL = load via `ReportDoc::from_bytes`,
///      then `ensure_blocks_layout` migrates a pre-#960 doc in place.
///   2. Project the doc to capture `(summary_before, body_before)` —
///      the authoritative pre-write state for the edit-log entry.
///   3. Apply `op` via `apply_persisted_report_op`. Any `if_rev` /
///      `if_doc_rev` check happens in there, against the CRDT truth
///      inside this transaction — a conflict aborts the tx, so nothing
///      is written and no events are emitted.
///   4. Project back to `(summary_after, body_after)` and re-serialize
///      a `TrackReportPayload` from those values rather than from the
///      op's inputs. The projection is what the JSON cache must mirror
///      so a future read sees the post-merge text rather than a
///      partially-applied input — under single-writer it is the same
///      bytes, but reading from the doc keeps the JSON-cache contract
///      ("CRDT is source of truth") true by construction.
///   5. Write both columns and emit both events in one tx — via
///      `write_with_actor_events_typed` so the events are persisted in
///      the same transaction as the row update (commit-then-emit
///      invariant preserved).
///
/// **Both events fire on every successful call, including content-equal
/// writes** (e.g. re-asserting the same body, or `report.edit` with
/// `old_string == new_string`). PR4's UI can filter no-op entries from
/// the timeline if it wants. Keeping the invariant "every successful call →
/// one `CardUpdated` + one `TrackReportEdited`" dead simple means downstream
/// consumers never have to second-guess whether an event is missing. On the
/// failure side the rule is just as simple, and is the transaction's rather
/// than this function's: nothing commits, so neither event exists.
///
/// `target.current_payload` is the payload as it was last seen by the
/// caller. It's used only as the seed for the first-time
/// `from_payload` branch — once `body_crdt` is non-NULL, the doc is
/// the source.
#[allow(clippy::too_many_arguments)]
async fn persist(
    repo: &dyn RouteRepo,
    events: &EventBus,
    write: &WriteContext,
    actor: ActorId,
    author: EditAuthor,
    target: ReportEditTarget,
    op: ReportDocOp,
    agent_message: Option<String>,
    lifecycle: Option<TrackLifecycle>,
    auto_promote_draft: bool,
    recorder_shadow: Option<Arc<dyn RecorderShadowProbe>>,
) -> Result<(Card, Option<BlockOpOutcome>), CalmError> {
    let ReportEditTarget {
        track,
        report_card,
        current_payload,
    } = target;
    let report_card_id = report_card.id.clone();
    let track_id = track.id.clone();
    let area_id = track.area_id.clone();
    let scope = EventScope::Card {
        card: report_card_id.clone(),
        track: track_id.clone(),
        area: area_id.clone(),
    };
    let track_scope = EventScope::Track {
        track: track_id.clone(),
        area: area_id,
    };
    let report_card_id_inner = report_card_id.clone();
    let track_id_for_event = track_id.clone();
    let (updated, _ids) = write_with_actor_events_typed::<(Card, Option<BlockOpOutcome>), _>(
        repo,
        None,
        events,
        write,
        move |tx| {
            let id = report_card_id_inner.as_str().to_string();
            let report_card_id = report_card_id_inner.clone();
            let track_id = track_id_for_event.clone();
            let scope = scope.clone();
            let track_scope = track_scope.clone();
            let current_payload = current_payload.clone();
            let op = op.clone();
            let actor = actor.clone();
            let agent_message = agent_message.clone();
            let recorder_shadow = recorder_shadow.clone();
            Box::pin(async move {
                let mut events: Vec<(ActorId, EventScope, Event)> = Vec::new();
                if auto_promote_draft
                    && let Some(auto_events) = auto_promote_draft_in_tx(tx, &track_id).await?
                {
                    events.extend(
                        auto_events
                            .into_iter()
                            .map(|event| (ActorId::Kernel, track_scope.clone(), event)),
                    );
                }
                if let Some(target) = lifecycle
                    && let Some(lifecycle_events) = apply_requested_transition_in_tx(
                        tx,
                        &track_id,
                        target,
                        &actor,
                        agent_message.clone().unwrap_or_default(),
                    )
                    .await?
                {
                    if let Some(probe) = recorder_shadow.as_ref() {
                        probe
                            .record(tx, RecorderShadowDecisionKind::TrackLifecycle)
                            .await?;
                    }
                    events.extend(
                        lifecycle_events
                            .into_iter()
                            .map(|event| (actor.clone(), track_scope.clone(), event)),
                    );
                }
                if let Some(probe) = recorder_shadow.as_ref() {
                    probe
                        .record(tx, RecorderShadowDecisionKind::ReportWrite)
                        .await?;
                }
                // 1. Load (or lazy-init) the CRDT doc for this card.
                //    Loaded docs may still carry the pre-#960 layout
                //    (`ROOT.body` Text, no block map) — migrate them
                //    in place, reusing the PR1-derived block ids from
                //    the payload JSON as the id hint. The migrated
                //    bytes are written back below in this same tx.
                let existing = card_body_crdt_get_tx(tx, &id).await?;
                let mut doc = match existing {
                    Some(bytes) => {
                        let mut doc = ReportDoc::from_bytes(&bytes).map_err(|e| {
                            CalmError::Internal(format!(
                                "track_report: load CRDT for card {id}: {e}"
                            ))
                        })?;
                        doc.ensure_blocks_layout(current_payload.blocks.as_deref())
                            .map_err(|e| {
                                CalmError::Internal(format!(
                                    "track_report: migrate CRDT block layout for card {id}: {e}"
                                ))
                            })?;
                        doc
                    }
                    // Safe: current_payload was read outside the tx,
                    // but is only consulted here when body_crdt is
                    // still NULL in-tx. SQLite's single-writer means
                    // no concurrent writer can have populated the
                    // blob between that read and this branch; once
                    // body_crdt is non-NULL we take the Some arm and
                    // ignore current_payload entirely.
                    None => ReportDoc::from_payload(&current_payload),
                };
                // 2. Capture the pre-write projection for the edit-log
                //    entry. A malformed doc surfaces as Internal here
                //    (never a panic).
                let (summary_before, body_before) = doc.project().map_err(|e| {
                    CalmError::Internal(format!("track_report: project CRDT for card {id}: {e}"))
                })?;
                // 3. Apply the requested op on the doc. `if_rev`
                //    checks happen in here, against the CRDT truth
                //    inside this transaction — a conflict aborts the
                //    tx (nothing written, no events emitted).
                let (outcome, doc_rev) = apply_persisted_report_op(&mut doc, &op, author)?;
                // 4. Project back — these are the authoritative values
                //    that go into the JSON cache. Since #960 PR2 the
                //    CRDT block map is the source of truth: `body` is
                //    the per-block concatenation and `blocks` is the
                //    doc's own snapshot (id/rev alignment already
                //    happened inside `ReportDoc::update`), so nothing
                //    is re-derived at the JSON layer.
                let (summary_after, body_after) = doc.project().map_err(|e| {
                    CalmError::Internal(format!(
                        "track_report: project CRDT post-op for card {id}: {e}"
                    ))
                })?;
                let mut projected_payload =
                    TrackReportPayload::new(summary_after.clone(), body_after.clone());
                projected_payload.doc_rev = doc_rev;
                projected_payload.blocks = Some(doc.blocks_snapshot().map_err(|e| {
                    CalmError::Internal(format!(
                        "track_report: snapshot CRDT blocks for card {id}: {e}"
                    ))
                })?);
                let blocks = projected_payload.blocks.as_deref().unwrap_or_default();
                let (declarations, block_diagnostics) =
                    calm_types::report_blocks::tasks::project_task_declarations(blocks);
                // 5. The row write and the task projection, in the one order
                //    both writers use — see `write_report_row_and_project_tx`,
                //    which is where that order is stated and where the
                //    structural door meets this one.
                let (updated, task_projection) = write_report_row_and_project_tx(
                    tx,
                    &id,
                    track_id.as_str(),
                    &projected_payload,
                    &mut doc,
                    &declarations,
                    &block_diagnostics,
                )
                .await?;
                //    Then two events tagged with the same card scope. Order
                //    matters here too: `CardUpdated` first so an existing
                //    subscriber that processes both events sees the generic
                //    "row changed" signal before the structured edit-log entry
                //    (matches the historical broadcast order before PR2 added
                //    the structured event).
                let report_edited = Event::TrackReportEdited {
                    track_id: track_id.clone(),
                    card_id: report_card_id,
                    author,
                    // Kept on the event wire for historical compatibility;
                    // the withdrawn proposal channel has no write point.
                    author_plugin_id: None,
                    edit_id: uuid::Uuid::new_v4().to_string(),
                    summary_before,
                    summary_after,
                    body_before,
                    body_after,
                    agent_message,
                };
                events.push((
                    actor.clone(),
                    scope.clone(),
                    Event::CardUpdated(updated.clone()),
                ));
                events.push((actor.clone(), scope, report_edited));
                if !task_projection.changed_keys.is_empty() {
                    events.push((
                        actor.clone(),
                        track_scope,
                        Event::PlanUpdated {
                            track_id,
                            changed_keys: task_projection.changed_keys,
                            agent_message: None,
                        },
                    ));
                }
                events.extend(task_projection.kernel_events);
                Ok(((updated, outcome), events))
            })
        },
    )
    .await?;
    Ok(updated)
}

// ---------------------------------------------------------------------------
// The two statements both writers share, and the order between them
// ---------------------------------------------------------------------------

/// Write the report card's row (JSON cache + CRDT bytes) and then reproject the
/// track's tasks, inside the caller's transaction.
///
/// **The order of the two statements is the contract.** The block-existence leg
/// of task projection resolves `refs` by reading the report card's *payload
/// cache*, so the cache must already hold this write's snapshot when the
/// projection runs. Swap them and a task block whose `refs` point at another
/// block of the same write is checked against the previously committed payload:
/// on a create that payload is the birth skeleton, so every ref into the new
/// content resolves to nothing and the task collects a `reference_missing`
/// diagnostic — silently, because a diagnostic is data, not an error. Any
/// projection failure still rolls the row write back with it; the transaction,
/// not this function, is what makes the pair atomic.
///
/// Private, and shared by exactly two callers: [`persist`] (its step 5) and
/// [`structural_init_report_tx`]. That is the whole of what an edit and a
/// structural initialization have in common — everything else about them
/// differs, which is why they are two functions and not one function with a
/// mode parameter.
async fn write_report_row_and_project_tx(
    tx: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    report_card_id: &str,
    track_id: &str,
    payload: &TrackReportPayload,
    doc: &mut ReportDoc,
    declarations: &[calm_types::report_blocks::tasks::TaskDeclaration],
    diagnostics: &[Vec<calm_types::report_blocks::tasks::Diagnostic>],
) -> Result<(Card, TaskProjectionOutcome), CalmError> {
    let payload_value = serde_json::to_value(payload).map_err(|e| {
        CalmError::Internal(format!("track_report: serialize projected payload: {e}"))
    })?;
    let patch = CardPatch {
        title: None,
        kind: None,
        sort: None,
        payload: Some(payload_value),
        deletable: None,
    };
    let updated = card_update_with_crdt_tx(tx, report_card_id, patch, doc.to_bytes()).await?;
    let projection = project_tasks_tx(tx, track_id, declarations, diagnostics).await?;
    Ok((updated, projection))
}
