# Bound presentation notifications and legacy terminal lookup cost

Outcome: unchanged native observations do not publish presentation changes, and
legacy terminal disposal finds its operations without scanning unrelated history.
Keep the combined patch below 1,000 added/deleted lines.

Review tier: L2, because this adds a database index and changes notification
publication next to persistent presentation identity. Two independent reviews
must check the final snapshot; released migrations remain byte-frozen.

## Design

- Keep native identity, erasure, receipt repair and completion precedence in
  conversation storage. Return an explicit changed flag alongside a stored row.
  Reusing the current presentation reference is unchanged; creating or repairing
  the reference is a change, even when its source row already existed.
  Commit changed content and its event together; broadcast after live cleanup.
- The content observer and partial-reply settlement retain live reply cleanup
  and publish only changed presentations. A cursor identifies display order,
  not a revision: updates at one cursor must still publish.
- Add a partial expression index over legacy worker terminal identity and id.
  Preserve the exact disposal predicate, all lifecycle phases, joins and writer
  transaction boundaries. Pipe workers continue through their declared paths.
  The index avoids unrelated JSON scans; it does not grant disposal permission.
- Do not change frontend invalidation, event wire contracts, task execution or
  terminal retention policy. Existing safety checks remain authoritative.

## Acceptance

- Production content ingress emits for start, completion and changed completion,
  but not identical replay or a late start after completion; live cleanup stays
  correct. A missing presentation is repaired and announced.
- EXPLAIN of the actual disposal query uses an expression equality search for
  terminal scope; migration also applies with pre-existing operation rows.
- Existing unresolved-launch and typed-execution disposal refusals remain green.
- Run focused regressions red before the fix, relevant existing tests, text and
  contract gates, quick Rust gates, and mutation-check replay notification assertions.
- No production database mutation, real Codex E2E or frontend changes.
