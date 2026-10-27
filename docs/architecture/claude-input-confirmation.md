# Exact provider input acknowledgements

Review tier: L2 because positive acknowledgement changes persisted input barriers.

## Outcome
A Claude input acknowledged by its validated session and exact replay UUID, or
an ACP input with an exact durable completed prompt receipt, must not require
manual Keep. A successful local write, session init, output item, process exit,
or stop certificate alone must never clear an uncertain input.

## Ownership
Extend the existing typed acceptance evidence and canonical fact writer. Claude
qualifies its replay in the existing validated stream consumer. ACP qualifies its
prompt response in its retained receipt owner (shared by OpenCode and Grok).
Use the original card/runtime/thread/logical-turn/submission/attempt/proof. The
canonical acknowledgement writer rechecks that identity and the frozen plan in
one transaction, including factual ingress on a failed current carrier. Preserve
human decisions and execution settlement independently. A late local dispatch
report must not downgrade a positive acknowledgement.

## Compatibility
Existing Unknown facts remain uncertain until new exact native evidence arrives;
there is no inference from transcript text and no production data rewrite.
Acceptance evidence gains a submission acknowledgement shape. Existing V5 frames
remain readable without conversion; frame and proof identities do not change.
There is no frame/proof version bump: adding an evidence arm does not alter the
frame layout or require upgrading unrelated stored frames. Rollback to a
binary that cannot decode the new evidence requires the pre-upgrade database
backup, discarding activity since that backup. Prefer forward repair. A downgrade
is unsafe after the first new acknowledgement; do not rewrite confirmed facts
back to Unknown.

## Verification
Reproduce through real canonical dispatch and protected display reads before
implementation. Cover exact and mismatched/missing replays, acknowledgement before
the dispatch return, normal continuation, ACP completed vs unknown receipts, and
failed current carriers. Run focused suites, mutation verification of evidence
matching, contract/text checks, quick Rust gates and two independent L2 reviews.

## Input feedback ownership
Canonical queue and delivery feedback have one typed read contract:
`pending_inputs` carries saved work; `active_input` carries the exact current
execution, native delivery evidence and Human decisions. A live invocation can
still be awaiting its native receipt; that fact is not an execution error.
The shared presentation selector consumes these declared identities and current
run readiness, retaining real uncertainty after settlement or runtime failure.

`blocked_reason` reports operational issuance/runtime failures only. It must not
reclassify queue, receipt or Human-resolution facts into a second delivery warning.
Removing that duplicate projection changes neither dispatch authority nor the
checkpoint, native evidence, retry barrier or wire schema. The presentation-only
follow-up is L1 and is verified at authenticated HTTP and browser entry points.
