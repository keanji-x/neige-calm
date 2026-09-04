-- #1384 — the `Idempotency-Key` → track binding for `POST /api/tracks`.
--
-- A track id is minted by `track_create_tx` (`let id = new_id()`), so it is
-- NOT a function of any request field. "Which track did this key create" can
-- therefore only be *remembered*, never recomputed — which is the whole
-- difference from the two conversation write mouths, whose card id is
-- `sha256(scope, Idempotency-Key)`.
--
-- Before this table the only row that remembered it was the `operations` row,
-- and `OperationRuntime::submit` writes that row only AFTER `adapter.validate`
-- succeeds. A daemon outage makes `PlannerHarnessStartAdapter::validate`
-- refuse, so the track, its two cards, its folder claim and its workspace were
-- all committed with nothing pointing at them, and the next request under the
-- same key minted a second track. This row is written INSIDE the same
-- `BEGIN IMMEDIATE` transaction as the id, so on the arm that writes it there
-- is no interval in which the track exists and the binding does not.
--
-- A sidecar table rather than columns on `tracks`: `tracks` is read through an
-- explicit `TRACK_SELECT_COLUMNS` list mapped onto `TrackRow`, so a column no
-- reader selects is dead weight on that surface; the binding needs three ids
-- (track, planner card, report card) so the resume arm re-derives nothing; and
-- `track_create_tx` has 39 call sites, all but one of which could never supply
-- the value.
CREATE TABLE track_create_idempotency (
  area_id          TEXT NOT NULL,
  idempotency_key  TEXT NOT NULL,
  track_id         TEXT NOT NULL,
  planner_card_id  TEXT NOT NULL,
  report_card_id   TEXT NOT NULL,
  created_at_ms    INTEGER NOT NULL,
  -- The cross-process wall. Two creates under one `(area, key)` cannot both
  -- commit a track: the second one's INSERT violates this and rolls its whole
  -- create transaction back. The in-process `conversation_first_message_locks`
  -- claim serializes the common case; this is what holds when that map does
  -- not (a second instance), and it is fail-closed by construction rather than
  -- by a check somebody has to remember to write.
  PRIMARY KEY (area_id, idempotency_key)
) WITHOUT ROWID;

-- Every column is NOT NULL because the row exists only once all five facts are
-- known — inside the create transaction, after `track_create_tx` returned and
-- with both card ids already minted. There is no half-binding state, so the
-- database refuses to represent one.
--
-- No FOREIGN KEY and no ON DELETE CASCADE, deliberately and fail-closed: if
-- the track a key created has been deleted, a replay must NOT mint a
-- replacement under a key that already means "that track". The row survives as
-- a tombstone, the track lookup misses, and the handler answers an error
-- rather than 201-with-a-different-track.
