-- Issue #1316 S3 — one concept (planner), one spelling.
--
-- `Spec` named an ACTOR — the agent that drives a track's lifecycle — with a
-- word English readers parse as "specification". The decisive evidence that
-- `planner` is the right target is already in this schema: `worker_sessions`
-- (migration 0045) has spelled its contract `'planner' | 'executor' |
-- 'validator'` since the day it was created. The storage layer has been calling
-- it planner all along; only the code layer said spec.
--
-- WHAT THIS MIGRATION DELIBERATELY DOES NOT RENAME
--
-- The report-block DSL's `declared_by` and `tombstoned_by` keep the value
-- `"spec"`, and `tasks.declared_by` keeps its `'spec'` DEFAULT.
--
-- Not a stylistic exemption — a carrier fact. Those values are written by
-- agents into report documents and PROJECTED back out of them into
-- `tasks.declared_by`. Rewriting the documents means rewriting `cards.payload`
-- (JSON) and `cards.body_crdt` (automerge bytes) in lockstep, which is a
-- content edit with its own history, not a migration — the same wall that
-- froze `neige://wave/` and `spawn: "in-wave"` in 0081. Renaming only the
-- column would be worse than leaving it: the projection would immediately
-- write `'spec'` back from the next stored block it read.
--
-- So the seam is stated rather than hidden: everything the KERNEL owns
-- (`cards.role`, `events.actor`, `events.payload.$.author`, column names) says
-- planner; the document vocabulary agents author says spec.

-- ---------------------------------------------------------------------------
-- 1. Column rename. Value-preserving.
-- ---------------------------------------------------------------------------
ALTER TABLE tracks RENAME COLUMN spec_task_ceiling TO planner_task_ceiling;

-- ---------------------------------------------------------------------------
-- 2. `cards.role`, and the ORDER here is load-bearing in a way 0080 and 0081
--    did not have to deal with.
--
--    `cards_role_validate_update` is a BEFORE UPDATE trigger that ABORTs when
--    the new role is outside `('worker','spec','reportcard','assistant')`. It
--    would therefore reject this migration's own UPDATE. The triggers have to
--    be dropped BEFORE the rewrite and recreated with the new vocabulary after
--    it — a self-inflicted deadlock if written in the obvious order.
--
--    `idx_cards_one_spec_per_track` is a UNIQUE PARTIAL index whose predicate
--    is the value being rewritten. Rebuilt after the UPDATE, like 0080's
--    `idx_tracks_one_chat_per_area`: built before, it would index zero rows and
--    enforce nothing.
-- ---------------------------------------------------------------------------
DROP TRIGGER cards_role_validate_insert;
DROP TRIGGER cards_role_validate_update;
DROP INDEX idx_cards_one_spec_per_track;

UPDATE cards SET role = 'planner' WHERE role = 'spec';

CREATE TRIGGER cards_role_validate_insert
BEFORE INSERT ON cards
WHEN NEW.role NOT IN ('worker', 'planner', 'reportcard', 'assistant')
BEGIN
  SELECT RAISE(ABORT, 'cards.role must be one of worker|planner|reportcard|assistant (#585, #1189, #1316)');
END;

CREATE TRIGGER cards_role_validate_update
BEFORE UPDATE OF role ON cards
WHEN NEW.role NOT IN ('worker', 'planner', 'reportcard', 'assistant')
BEGIN
  SELECT RAISE(ABORT, 'cards.role must be one of worker|planner|reportcard|assistant (#585, #1189, #1316)');
END;

CREATE UNIQUE INDEX idx_cards_one_planner_per_track ON cards(track_id) WHERE role = 'planner';

-- ---------------------------------------------------------------------------
-- 3. `events.actor`. Four spellings exist — the bare and the `:<card-id>`
--    suffixed form, for each of the card and session actors
--    (`calm_types::ids::Actor::Display`) — so there are four statements, and
--    every one of them is ANCHORED: exact equality for the two bare forms, and
--    `LIKE 'ai:spec…:%'` for the two suffixed ones, which requires the
--    separating colon to be present.
--
--    Anchoring is not tidiness here, it is the difference between a rename and
--    data corruption. The obvious `LIKE 'ai:spec%'` prefix test also matches
--    values that are none of the four: `actor::validate_header_actor` accepts
--    any `ai:[a-z0-9-]{1,64}`, so a client can persist `ai:specialist`, and an
--    unanchored rewrite silently turns it into `ai:plannerialist`. Verified by
--    running the statements below against `ai:special`, `ai:specter:9` and
--    `ai:spec-sessionfoo`: none of the three matches anything.
--
--    Anchoring also removes the ordering constraint an unanchored version would
--    have had (`ai:spec-session` matching a bare `ai:spec` prefix): with these
--    four predicates the statements are disjoint, so their order is free.
-- ---------------------------------------------------------------------------
UPDATE events SET actor = 'ai:planner-session'            WHERE actor = 'ai:spec-session';
UPDATE events SET actor = 'ai:planner-session:' || substr(actor, length('ai:spec-session:') + 1)
 WHERE actor LIKE 'ai:spec-session:%';
UPDATE events SET actor = 'ai:planner'                    WHERE actor = 'ai:spec';
UPDATE events SET actor = 'ai:planner:' || substr(actor, length('ai:spec:') + 1)
 WHERE actor LIKE 'ai:spec:%';

-- ---------------------------------------------------------------------------
-- 4. `events.payload.$.author` — `calm_types::EditAuthor`, a bare-lowercase
--    unit variant persisted inside the `track.report_edited` payload JSON.
--    Rewritten with JSON1 rather than a string REPLACE so a body that merely
--    mentions the word cannot be corrupted.
--
--    `events.payload` has no `json_valid` CHECK (unlike `operations`), and a
--    non-JSON body makes `json_extract` abort the whole migration — measured.
--    The guard is a CASE rather than a second `AND` because SQLite does not
--    promise left-to-right evaluation of WHERE conjuncts, so an `AND
--    json_valid(...)` sibling is not actually a guard; CASE orders it.
--
--    No `event_version` bump accompanies section 3 or 4: unlike 0080/0081,
--    neither rewrites an event KIND, so a client's per-frame discriminator gate
--    still recognises these rows. What changes is a field's value, and the
--    frontend's own bundle is replaced in lockstep by the WEB_COMPAT_VERSION
--    bump this slice carries.
-- ---------------------------------------------------------------------------
UPDATE events
   SET payload = json_set(payload, '$.author', 'planner')
 WHERE kind = 'track.report_edited'
   AND CASE WHEN json_valid(payload)
            THEN json_extract(payload, '$.author')
       END = 'spec';

-- ---------------------------------------------------------------------------
-- 5. `operations.kind`.
--
--    Operations are NOT immutable history — a row can be pending or parked
--    across a restart and then resumed, so its dispatch key has to survive the
--    rename. `kind` is how the driver finds an adapter
--    (`operation/driver.rs::adapter` keys a `HashMap<&'static str, _>` on it)
--    and how `session_repo_impl.rs:218` locates a track's planner session.
--    Leaving it means a parked `spec-harness-start` can never be resumed, AND
--    the session lookup matches zero rows for every pre-upgrade track, so live
--    planner sessions read as dormant.
--
--    `operations.payload_json` is deliberately NOT rewritten. Its field names
--    are frozen instead, in the struct itself — see the doc comment on
--    `PlannerHarnessStartOperationPayload`. A rewrite here would desync the row
--    from its own `payload_hash`, which is a permanent 409 on every stable
--    idempotency key (Today's launchpad, the scheduler's child bootstrap) with
--    no self-healing path, because nothing deletes from `operations`.
--
--    `operations` carries `UNIQUE (kind, idempotency_key) WHERE
--    idempotency_key IS NOT NULL`, so a kind rewrite could in principle collide.
--    It cannot here, and the argument is checkable rather than assumed: the
--    three `planner-harness-*` strings do not appear anywhere in the tree before
--    this slice (`git grep planner-harness- origin/main` is empty), so no build
--    that has ever run could have written a row under those names.
-- ---------------------------------------------------------------------------
UPDATE operations SET kind = 'planner-harness-start'     WHERE kind = 'spec-harness-start';
UPDATE operations SET kind = 'planner-harness-interrupt' WHERE kind = 'spec-harness-interrupt';
UPDATE operations SET kind = 'planner-harness-shutdown'  WHERE kind = 'spec-harness-shutdown';
