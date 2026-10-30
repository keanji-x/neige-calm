# Claude identity after a lost bind

A matching Claude init can precede a failed durable session bind. Without an
accepted exact replay, a restarted kernel cannot tell whether the native session
exists. Reusing `--session-id` or guessing `--resume` is not a recovery contract.

## Durable launch boundary

A new Planner turn records its worker, card, Track, thread and execution in
`claude_session_launches` in the same transaction as execution admission. A failed
record write admits no process. A successful live bind or verified exact-replay
recovery removes the record in the binding transaction.

An unconfirmed record holds subsequent issuance before any line is sent. A record
for another thread is stale; a record crossing its card or Track refuses. An old
completion can retire only its own execution's record. Retirement without init
requires a settled end and a clean, fully read stdout stream; interrupted, missing
or invalid output retains uncertainty. A refusal before the launcher's durable
release can also retire its own record because Claude has not run.

## Explicit recovery

Canonical conversations use the existing authenticated
`POST /api/cards/{id}/conversation/abandon` action and its exact
carrier/submission/attempt/revision comparison. Claude eligibility is independent
of an active input: Human Keep or a later preflight refusal does not make the
identity ambiguity disappear. A matching launch must name a registered execution.
ACP continues to require its own unresolved native journal receipt.

The provider stops and settles its registered execution, requires the boot-owned
absence proof for older processes, then captures the complete execution history.
The common proof checks consumer, Track, alias, execution identity and complete
descendant settlement, and compares that evidence again at commit. An absent
execution claim alone proves nothing.

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

Migration `0203_claude_session_launches.sql` adds a provider-owned table without
backfill. Sessions whose binding was already lost before launch records existed
remain outside this prevention guarantee. No Claude filesystem layout, stderr or
transcript text is treated as identity evidence. This change does not establish
real-provider E2E or close the broader integrated acceptance in #3331.
