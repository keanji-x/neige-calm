# Canonical queued-message controls

Restore the existing queued Human message experience on the canonical input
lifecycle: list saved pending messages, edit or withdraw them, steer one into
the exact running Codex turn, and retain a truthful current-response status.

Review tier: L2. This changes persisted submissions and current-source authority.

## Ownership and contracts

- Truth keeps the sole ordered checkpoint. Pending edits append an immutable
  revision and replace its exact reference without moving its queue position;
  withdrawal marks the message and removes that reference in one transaction.
  Only the original current author can change a still-pending exact revision.
- Extend the existing submission relation to one Initial at ordinal zero and
  Supplemental submissions at positive ordinals. Do not add a second queue or
  copy message bodies into the checkpoint. Released migrations stay byte-frozen.
- Conversation input admission owns author/resource checks, claim and live
  source revalidation. Native transport owns delivery evidence. Supplemental
  work binds the original native execution and uses existing crossing markers;
  Unknown never authorizes retry or delivery to a later turn.
- Supplemental claim and crossing marker commit in one transaction, avoiding
  an unattempted claim after cancellation. Keep Initial selection guards intact.
  Validate the retained original target independently of END eligibility, which
  waits for supplemental crossing reports. END acknowledges only Initial;
  supplemental uncertainty survives as retained unresolved attempt evidence.
  An independently owned steer task drains its native ticket/report after an
  HTTP disconnect. Required submission ordinals expose supplemental uncertainty
  even while Initial is running. Keep/Retry before the final native gate may
  produce definite non-dispatch; resolved supplemental facts must not hold the
  original END or automatically requeue the predecessor. Settlement alone
  returns exact consumed source identities, including those resolved races.
- Source-index edits observe the new exact revision and retire only the previous
  committed identity. Withdrawals retire only their committed exact identity.
  Preserve attachments during text edits. Allocate supplemental ordinals from
  persisted submissions, including released ones, never from active-list length.
- Confirmed non-dispatch releases only that Supplemental claim at its original
  queue position, retaining the original running execution. END and recovery
  retain/settle all submissions according to their own delivery evidence.
- The protected read publishes only current-author pending Human rows through
  the existing queue view; System/retry references remain separate counted
  inputs. Reuse the queued composer, actions, error feedback and motions.
  Never infer author identity from body text or a native echo.
- Canonical edit/withdraw/steer use message-specific HTTP ports with required
  carrier/revision identities and an exact native turn for steer. They consume
  the canonical transaction owner, with no historical fallback on failure.
  Historical ports and behavior stay intact.
- Pending inputs have explicit waiting feedback even without a live turn or a
  terminal outcome. Current terminal facts remain scoped to the execution.

## Acceptance and verification

Exercise production authenticated HTTP acceptance and the native protocol peer:
queue multiple messages, edit/withdraw one, steer one, consume END, reopen.
Check order, immutable prior versions, no duplicate transcript carriers, own
author/revision/carrier checks, claim-vs-edit races, END-vs-steer races, definite
non-dispatch, uncertain delivery, restart, and unsupported-provider refusal.
Migration checks preserve populated claims/retries/presentations and FK integrity.
Browser checks use existing controls in desktop/mobile, retained composer edits,
cross-view refresh, waiting/running/terminal states and provider capabilities.

Run smallest regression tests red before implementation, focused Rust/frontend
tests while iterating, required text/contract/Rust quick and frontend checks,
relevant browser/E2E tests. Mutation-verify authority/no-resend assertions in
the exclusive worktree. Two independent read-only L2 reviews must converge.
No production deployment is part of implementation verification.
