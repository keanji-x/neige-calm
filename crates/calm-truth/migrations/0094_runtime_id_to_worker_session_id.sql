-- Issue #1316 S4b — retire the `runtime_id` spelling from the carriers the
-- kernel owns and types.
--
-- The word names `worker_sessions.id`. It has named nothing else since
-- migration 0055 dropped the `runtimes` table; every `runtime_id` left in the
-- tree is a value read out of, or written into, `worker_sessions`. S4a
-- (`0086`) did the same job for `worker_flow_items`. This migration finishes
-- the kernel-owned surface: one column, three event KINDS, and one payload key
-- across nine event kinds.
--
-- ---------------------------------------------------------------------------
-- 1. `harness_items`: `runtime_id` -> `worker_session_id`.
--
--    `RENAME COLUMN` is value-preserving, so every row keeps exactly what it
--    had, and SQLite rewrites the definitions of any index that mentions the
--    column automatically. The `DROP INDEX` below is therefore NOT required
--    for correctness — nothing breaks if it is omitted. It exists to retire
--    the index NAME, which encodes the retiring word and which SQLite has no
--    `ALTER INDEX RENAME` for.
--
--    Dropping rather than recreating under a new name: no query in the tree
--    can use this index. Every access path into the table is by `card_id` or
--    by `id` —
--      * `read.rs::harness_item_page` (the only SELECT) filters
--        `WHERE card_id = ?1 AND id <cmp> ?2`, served by
--        `idx_harness_items_card_id`;
--      * `out_of_domain.rs::harness_items_measure_by_card_tx` and
--        `harness_items_delete_by_card_tx` are both `WHERE card_id = ?1`;
--      * `out_of_domain.rs::harness_item_insert` is the only writer;
--      * the FK added by `0032` is on `card_id`.
--    Nothing joins or filters on this column. Same reasoning, and the same
--    outcome, as `0086`.
-- ---------------------------------------------------------------------------
DROP INDEX IF EXISTS idx_harness_items_runtime_id;

ALTER TABLE harness_items RENAME COLUMN runtime_id TO worker_session_id;

-- ---------------------------------------------------------------------------
-- 2. Event KINDS. `runtime.*` -> `worker_session.*`.
--
--    These three are the discriminator a client gates each frame on, so the
--    `event_version` stamp travels with them exactly as in `0080` (the rename
--    to `area`, v14) and `0081` (the rename to `track`, v15): a client that
--    does not know the new tag must be held behind the version gate rather
--    than silently dropping frames it cannot classify. 16 is
--    `SYNC_EVENT_VERSION` after this slice —
--    `scripts/gate-sync-event-version-lockstep.sh` fails the build if the two
--    ever disagree.
-- ---------------------------------------------------------------------------
UPDATE events SET kind = 'worker_session.started',        event_version = 16 WHERE kind = 'runtime.started';
UPDATE events SET kind = 'worker_session.status_changed', event_version = 16 WHERE kind = 'runtime.status_changed';
UPDATE events SET kind = 'worker_session.superseded',     event_version = 16 WHERE kind = 'runtime.superseded';

-- ---------------------------------------------------------------------------
-- 3. Event payload KEYS, for the nine kinds that carry one.
--
--    Three shapes:
--      * `worker_session.started` / `.status_changed` (matched on their
--        POST-rename names, because section 2 has already run) and the four
--        `harness.*` kinds carry a single `runtime_id` at the payload root;
--      * `worker_session.superseded` carries TWO root keys (`old_runtime_id`
--        and `new_runtime_id`), moved by two independently guarded statements;
--      * `card.added` / `card.updated` carry it nested inside the card's
--        `runtime` object (`calm_types::model::CardRuntimeView`), at
--        `$.runtime.runtime_id`, NOT at the payload root. That object is
--        `Option`al and skipped when absent, so most card rows have no
--        `runtime` key at all.
--
--    `json_set` ADDS a key, it does not rename one — see `0014`'s header, which
--    documents the same trap. So every statement is
--    `json_remove(json_set(...))`: copy to the new path, then delete the old.
--
--    TWO GUARDS, both load-bearing.
--
--    (i) EXISTENCE. `json_extract` on a missing path returns SQL NULL, and
--        `json_set(payload, '$.new', NULL)` writes a JSON `null` — i.e. an
--        unguarded statement would FABRICATE `worker_session_id: null` on every
--        row it touched that never had the key. `json_type(payload, '$.path')`
--        returns SQL NULL for a missing path and the text `'null'` for a
--        present-but-null value, so `IS NOT NULL` is the exact "the key is
--        there" test. A row that lacks the key matches nothing here and comes
--        out byte-identical, `event_version` included.
--
--    (ii) VALIDITY. `events.payload` has no `json_valid` CHECK (unlike
--        `operations`, whose three JSON columns all carry one), and
--        `json_extract` on a non-JSON body ABORTS the whole migration. The
--        guard is a `CASE`, not an `AND json_valid(...)` conjunct: SQLite does
--        not promise left-to-right evaluation of WHERE conjuncts, so an `AND`
--        sibling is not actually a guard. `CASE` orders it. Precedent and
--        measurement: `0083` §4.
-- ---------------------------------------------------------------------------

-- 3a. Root-level `runtime_id`, six kinds (the remaining three of the nine are
--     `worker_session.superseded` in 3b and the two `card.*` in 3c).
UPDATE events
   SET payload = json_remove(
                   json_set(payload, '$.worker_session_id', json_extract(payload, '$.runtime_id')),
                   '$.runtime_id'),
       event_version = 16
 WHERE kind IN (
         'worker_session.started',
         'worker_session.status_changed',
         'harness.item.added',
         'harness.phase.changed',
         'harness.transcript.cleared',
         'harness.user_message.enqueued'
       )
   AND CASE WHEN json_valid(payload)
            THEN json_type(payload, '$.runtime_id') IS NOT NULL
       END;

-- 3b. `worker_session.superseded` carries both ids. Two statements, each
--     guarded on its own key, so a row holding only one of them cannot have
--     the other fabricated as null.
UPDATE events
   SET payload = json_remove(
                   json_set(payload, '$.old_worker_session_id', json_extract(payload, '$.old_runtime_id')),
                   '$.old_runtime_id'),
       event_version = 16
 WHERE kind = 'worker_session.superseded'
   AND CASE WHEN json_valid(payload)
            THEN json_type(payload, '$.old_runtime_id') IS NOT NULL
       END;

UPDATE events
   SET payload = json_remove(
                   json_set(payload, '$.new_worker_session_id', json_extract(payload, '$.new_runtime_id')),
                   '$.new_runtime_id'),
       event_version = 16
 WHERE kind = 'worker_session.superseded'
   AND CASE WHEN json_valid(payload)
            THEN json_type(payload, '$.new_runtime_id') IS NOT NULL
       END;

-- 3c. `card.added` / `card.updated` — nested under the card's `runtime` view.
UPDATE events
   SET payload = json_remove(
                   json_set(payload, '$.runtime.worker_session_id', json_extract(payload, '$.runtime.runtime_id')),
                   '$.runtime.runtime_id'),
       event_version = 16
 WHERE kind IN ('card.added', 'card.updated')
   AND CASE WHEN json_valid(payload)
            THEN json_type(payload, '$.runtime.runtime_id') IS NOT NULL
       END;

-- ---------------------------------------------------------------------------
-- 4. `operations` is deliberately NOT touched. There is no `UPDATE operations`
--    in this file, and the Rust-side fields are renamed but PINNED to the old
--    wire key with `#[serde(rename = "runtime_id")]`.
--
--    Two reasons, each checked rather than assumed:
--
--    (1) HETEROGENEOUS PATH SHAPES. The word appears in all three of the
--        table's JSON columns, at a different depth in each:
--          * `payload_json`      — at the root (the six create/interrupt/
--            shutdown operation payload structs);
--          * `tx_output_json`    — under `$.data` (`TxOutput::data`, read back
--            by `TxOutput::output_string("runtime_id", ..)`);
--          * `compensation_state` — under `$.steps[*].args`, a
--            VARIABLE-LENGTH array (`CompensationStateVersioned::steps`, read
--            back by `CompensationStep::arg_string("runtime_id", ..)`).
--        The third has no fixed path, so it needs `json_each` and a rebuild,
--        not a `json_set`.
--
--    (2) THE `rename` IS WHAT KEEPS IN-FLIGHT ROWS READABLE. `operations` is
--        not immutable history: a pending or parked row is resumed across a
--        restart and its `payload_json` is re-read by the adapter then — the
--        same property that forced `0083` §5 to rewrite `operations.kind`
--        while leaving `operations.payload_json` frozen. So renaming the Rust
--        field WITHOUT `#[serde(rename)]`, and without rewriting the column,
--        would silently stop reading the stored key. For the four
--        `Option<String>` payloads (codex/claude/claude-restart/terminal
--        create) a row carrying a REAL id would deserialize to `None` and
--        `payload.worker_session_id.clone().unwrap_or_else(new_id)` would mint
--        a brand-new session id, re-running the spawn against a session that
--        never existed. For the two bare-`String` payloads
--        (`PlannerHarnessInterruptOperationPayload`,
--        `PlannerHarnessShutdownOperationPayload`) there is no default and no
--        fallback, so the same row would fail to deserialize at all and wedge
--        the parked operation. Silent corruption on one side, a wedge on the
--        other; neither is worth a spelling. Pinning the wire key is the
--        cheap, reversible half of the trade — the rename this slice is about
--        is a Rust-identifier rename, which costs the stored rows nothing.
--
--    `operation/repo_sqlite.rs`'s string-keyed `payload.get("runtime_id")`
--    lookup is left alone for the same reason: it reads `payload_json`.
-- ---------------------------------------------------------------------------
