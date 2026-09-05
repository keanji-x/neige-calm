# Goal and inputs

- Template input: the track's bound `template_input` JSON is the task's source of truth,
  not the track title.
- Ingest (inspect-issue): derive the track goal from gh.issue.view on input.repo /
  input.issue_number. Record the issue's requirements and constraints in the track
  report before dispatching any downstream task.
- notes: optional advisory context from the requester; it never overrides the issue or
  the gates.

## Check the repository

Repo cross-check (inspect-issue acceptance): before any write action, compare input.repo
against `git remote get-url origin` run in the track cwd (owner/name after stripping the
host and a trailing .git). On mismatch do NOT proceed: move working->blocked via
calm.ratify.request with `reason:"repo_mismatch: input.repo=<owner/name>, cwd.origin=<owner/name>"` (that exact prefix, then both observed values), and wait for
the human decision.

# Plan

Pre-set issue-development plan. Treat the `task` blocks as the authoritative plan.
Activate by replacing those task blocks and setting `ready: true` — use the read's block
ids and revision as replace anchors. Do not mint duplicate tasks. Prose blocks are NOT a plan to activate: maintain them per this document's own contract.

# Review convergence

For this track, drive dual-review convergence for each review subject.

## Record both verdicts

- After BOTH channels for a phase complete, call calm.review.round with
  subject:{phase,slice_id,pr_number?}, optional head_sha, n, cap, converged,
  channels:[both verdicts], and root_cause when known.
- Record each channel's verdict as the literal lowercase token `approved` or
  `changes_requested` (exactly those strings).
- converged is true only when EVERY channel verdict is `approved`.
- For PR subjects, head_sha is the reviewed forge.pr.diff.read head_sha; omit head_sha
  for design subjects.

Record root_cause each round; repeated facets should drive a class fix.

## Review rounds and fixes

- For each subject, set n to the last observed review.round n for that same subject plus 1.
  cap is the fixed policy constant 8 for a subject's first review window; after a
  cap-exhaustion ratify grant it is the previous cap plus exactly 2 (see ASK-HUMAN
  below).
- Always re-review. Every fix re-dispatches BOTH channels before the next
  calm.review.round.

## When the review limit is reached

If n == cap and the round is non-approving, do not merge.

- Either GIVE-UP by recording the terminal rationale in the report with
  calm.report.write and lifecycle failed for reviewing->failed; OR ASK-HUMAN by first
  moving reviewing->working with the normal lifecycle arg, then call calm.ratify.request
  with `reason:"cap_exhausted"` for working->blocked.
- On ratify.resolved grant the track is already back in working; resume
  working->reviewing and continue reviewing the exhausted subject with cap = previous
  cap + 2 on its next round.
- The kernel accepts this raise at most once per subject per grant; a grant may
  authorize this for each subject that was already cap-exhausted when it was issued.
- If the extended window also exhausts without convergence, GIVE-UP or ASK-HUMAN again.

# Verification gates

gates: author each agent task's `gate` from the TARGET repo's own toolchain — detect it
(Cargo / npm / pytest / go / Make, etc.) and run that ecosystem's formatter, linter, and
tests where present; do not hardcode `cargo test`.

# Merge and approval

## Merge fence F4

Merge fence F4: call gh.pr.merge for a subject ONLY when that subject's latest
review.round has converged:true. Pass expected_head_sha equal to that round's head_sha.

## Merge policy

- merge_policy: `auto-merge` allows gh.pr.merge as soon as merge fence F4 is satisfied.
- `hold-for-ratify` — also the semantics whenever merge_policy is absent — additionally
  requires a granted ratify BEFORE gh.pr.merge.
- Drive everything up to converged reviews + green checks, then move reviewing->working
  with the normal lifecycle arg (calm.ratify.request 400s outside working), and call
  calm.ratify.request with `reason:"merge_hold: pr #<n> converged at <head_sha>"` for
  working->blocked.
- On ratify.resolved grant the track is already back in working: the grant authorizes
  merging that already-converged head — no fresh review round is required for the hold
  itself; resume working->reviewing and call gh.pr.merge per fence F4 (expected_head_sha
  = the converged round's head_sha).
