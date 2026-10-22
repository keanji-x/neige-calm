# Canonical Planner compact tasks

Review tier: L2. Kernel automation authority and atomic task settlement cross
execution and persistence boundaries. Planner arranges two independent reviews.

A declared `compact` task works for canonical Codex and Claude Planners. The task
row is the durable request. Its execution ID is the existing compact outcome's
operation ID; the V5 runtime header and frame format remain unchanged.

The scheduler resolves the current canonical runtime rather than refusing the
conversation. Canonical idle admission leaves the dispatched task intact until
Start commits, so pending input and rewind take precedence. Start marks the task
running in the frame transaction. Settlement completes it or fails it with
`compaction-incomplete`, and appends one wake in that same transaction. The wake
uses the claim-frozen handoff and the existing historical task/wake helpers.

Native maintenance reuses the validated application's existing
`LegacyPrivateExecution` declaration and its Area execution policy. It does not
construct a Human session or grant membership. The installation registry retains
only this live, nonserializable issuer; the task row remains the sole request
record. The declaration becomes available at the existing post-recovery boot
boundary. Manual maintenance retains its actual Human issuer.

Restart wedges Started maintenance and settles its task without resubmission.
The existing observation budgets bound unknown start and missing END separately.
Late END and delayed scheduler sweeps cannot settle or wake a terminal task twice.

Verification ownership: the worker runs focused fixture tests, formatter fixes,
and mutation verification of settlement rollback on wake failure. The kernel gate
runs ratchet, contract, Rust quick and one focused replay; CI owns broad suites.
No permanent test-name registry or provider-specific scheduler policy is added.
An ordinary added test changes only its owning test module; an added provider
implements the backend maintenance contract. Each declared gate runs once per
successful workflow, with the intended focused replay in addition to local tests.
