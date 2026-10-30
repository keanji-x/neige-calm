# Claude identity after a lost bind

A matching Claude init can precede a failed durable session bind. Without an
accepted exact replay, a restarted kernel cannot tell whether the native session
exists. Reusing `--session-id` or guessing `--resume` is not a recovery contract.

## Durable launch boundary

A new Planner turn records its worker, card, Track, thread and execution in
`claude_session_launches` in the same transaction as execution admission. A failed
record write admits no process. A successful live bind or verified exact-replay
recovery removes the record in the binding transaction. Read-only recovery
declarations reuse that exact evidence validator, so an accepted replay does not
become an identity hold merely because its live bind failed. A still-live host
can also retain a matching observed init in memory and retry the bind on its
next resume. Recovery display consumes that provider-owned fact; it is not an
authorization or END proof, and a recreated host must use durable evidence.

An unconfirmed record holds subsequent issuance before any line is sent, including
the separate native edit-preparation probe. A record
for another thread is stale; a record crossing its card or Track refuses. An old
completion can retire only its own execution's record. Once released, a process
may have created the native session before emitting init, even when it exits with
complete stdout. Its record is retained until a trusted bind/replay confirms it or
the person explicitly retires the carrier. Only a refusal while the launcher's
durable release is still Held proves that Claude has not run and can retire an
unbound launch record.

## Explicit recovery

Canonical conversations use the existing authenticated
`POST /api/cards/{id}/conversation/abandon` action and its exact
carrier/submission/attempt/revision comparison. Claude eligibility is independent
of an active input: a lost dispatch report (`CrossingUnknown`), Human Keep or a
later preflight refusal does not make the
identity ambiguity disappear. A matching launch must name a registered execution.
ACP continues to require its own unresolved native journal receipt.

The provider stops and settles its registered execution, requires the boot-owned
absence proof for older processes, then captures the complete execution history.
The common proof checks consumer, Track, alias, execution identity and complete
descendant settlement, and compares that evidence again at commit. An absent
execution claim alone proves nothing.

The registry owner is captured after the card start fence has joined any pending
retained installation, including a wake caused by Keep or accepted Human input.
The existing recovery owner retains Human authorization, card/Track lifecycle
locks, registry retirement protection, MCP and ask fences, the full checkpoint and
ingress comparisons, and credential revocation. The old carrier's attempts,
receipts and transcript remain factual history. Human pending inputs retain their
identity and order. Worker-bound System and retry intents block replacement;
recovery does not silently transfer or discard their authority.

The fresh carrier remains dormant with no native thread and sends nothing.
Only a later authorized input can activate it on a fresh UUID. Historical
conversations retain their existing `/planner/restart` route.

## Upgrade and limits

REST revision 43 and web compatibility floor 66 require older bundles to refresh before reading the
new `identity_unconfirmed` recovery reason. The action endpoint already exists;
new clients continue to consume only server-declared recovery actions.

Migration `0203_claude_session_launches.sql` adds a provider-owned table without
backfill. Sessions whose binding was already lost before launch records existed
remain outside this prevention guarantee. No Claude filesystem layout, stderr or
transcript text is treated as identity evidence. This change does not establish
real-provider E2E or close the broader integrated acceptance in #3331.
