-- #1428 — an `operations` row carrying an `idempotency_key` is the
-- `OperationRuntime::submit` dedup wall, and is therefore permanent.
--
-- WHY THIS IS A FENCE AND NOT A COMMENT. #1384 §6.4 recorded a future
-- `operations` reaper as degrading the track-create replay arm — "a behaviour
-- change, not a correctness hole". That is FALSE, and the whole reason this
-- migration exists. Reaping a *succeeded* keyed row makes the next
-- byte-identical replay deliver the user's first message a SECOND time:
--
--   1. `retryable_operation_key` returns the first key that is absent or
--      non-`Failed`, so the reaped base key reads absent and is returned
--      (`calm-server/src/routes/conversations_shared.rs`).
--   2. `select_arm(binding_hit = true, chosen_is_occupied = false)` is
--      `GenuineRetry` (`calm-server/src/routes/tracks/create.rs`).
--   3. `GenuineRetry` resubmits the first message — both resuming arms pass
--      `plan.text` to `start_planner_harness_with_first_message`.
--   4. `submit` short-circuits ONLY when `find_by_idempotency_key` finds an
--      existing row (`calm-server/src/operation/driver.rs`). It was reaped, so
--      it does not.
--   5. `PlannerHarnessStartAdapter::validate` takes the non-minting branch and
--      passes: the planner card exists, which is exactly what that branch
--      requires.
--   6. `prepare_tx` pushes `Observation::UserMessage` unconditionally — there
--      is no "already enqueued" predicate on this path — supersedes the active
--      runtime, and writes a second `harness.user_message.enqueued`.
--
-- THE CRITERION IS CO-EXTENSIVE WITH THE WALL, not a heuristic about which
-- kinds matter. `submit` consults `find_by_idempotency_key(kind, &key)` for
-- EVERY operation kind, and that lookup matches on this column. So a row with
-- a non-NULL `idempotency_key` is by construction a live short-circuit for any
-- future `submit` under its `(kind, key)`, and a row with a NULL one can never
-- be found by that lookup and so can never change a `submit` decision. There
-- is no row inside the criterion that is not a wall, and none outside it that
-- is.
--
-- WHAT A LEGITIMATE REAPER MAY STILL DO, with no migration: delete rows whose
-- `idempotency_key IS NULL`. That is the ordinary shape retention wants
-- (worker spawns and other unkeyed operations), and the `WHEN` clause below
-- leaves it open on purpose.
--
-- THE BARE-DELETE PROPERTY IS LOAD-BEARING, so do not "optimize" this trigger
-- away on the grounds that no caller deletes today. A row trigger disables
-- SQLite's truncate optimization, so `DELETE FROM operations` with no WHERE
-- clause fires this per row and aborts too. That is what makes this a fence
-- rather than a tripwire on a spelling: it cannot be evaded by a query
-- builder, by a different capitalization, or by omitting the WHERE.
--
-- IF YOU NEED TO REBUILD THIS TABLE (the rename → create → copy → drop shape
-- migration 0042 used), note that SQLite drops a trigger silently along with
-- its table and fires no delete trigger on DROP TABLE. Your rebuild migration
-- MUST recreate this trigger in the same file. `head_schema_has_the_keyed_
-- operations_fence` fails closed if you forget, and says why.
CREATE TRIGGER operations_keyed_rows_are_permanent
BEFORE DELETE ON operations
WHEN OLD.idempotency_key IS NOT NULL
BEGIN
  SELECT RAISE(ABORT, 'refusing to delete an operations row that carries an idempotency_key: that row is the submit() dedup wall, and deleting it lets the next byte-identical retry re-run the operation and deliver its message a second time. A retention pass may delete rows WHERE idempotency_key IS NULL; keyed rows are permanent. See docs/design-1428-idempotency-retention.md section 3.');
END;
