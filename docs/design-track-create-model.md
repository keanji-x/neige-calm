# Track creation model selection

POST /api/tracks accepts optional nullable `model` and `reasoning_effort`
strings. Omission and null follow installation defaults. Reuse the conversation
model endpoint's catalog advice (unknown models allowed). Unlike PUT model,
create has no adjustment flags in its Track response: when current catalog
advice would adjust an unsupported effort, refuse with 400 before any mint,
naming the requested effort, model, and catalog default. This also handles a
client selecting from a stale roster without silently changing its first turn.

Hash raw non-null selections before replay lookup, excluding both keys when
unset to preserve existing durable default-request fingerprints. Resolve catalog
advice only on mint, so mutable catalog state cannot invalidate a retry.
Apply CardModelSelection to the requested initial planner payload in the existing create
transaction, before CardAdded and before harness startup. No migration or second
write is needed. The subsequent track-detail read exposes the stored planner selection; the
create response remains Track and does not add adjustment flags.

Acceptance: HTTP first-message creation starts its first turn with the selected
pair; stale unsupported effort returns 400 with no new track/card/binding;
keyed retries reject changed model or effort; identical retries replay;
default/null requests retain old digest bytes. Exercise the real HTTP route and
fake daemon recording, plus focused existing replay and model endpoint tests.

Existing transaction authorization already refuses agent creates with explicit
model or effort selections, rolling back all rows. A focused HTTP regression
confirms both shapes leave no tracks/cards; no additional actor rule is needed.

## API ownership change (#1610)

The requested creation controls require adding the two optional request fields
above. The `core/api` change is limited to the generated OpenAPI contract;
regenerate it from the server schema rather than changing a frozen hand-written
interface. This records the API change approved by the task's requested scope.
The current cross-Area authorization fingerprint remains intact alongside the
new model and effort entries.

OWNERSHIP-CHANGE: fe/core/api/generated/openapi.json — expose model and reasoning-effort overrides on track creation (#1610)
