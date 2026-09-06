# Recover an Area creation after a lost response (#1500 A2)

An acknowledged retry of one creation intent must resolve to one Area, even
when the first response disappears after its transaction commits. Area names
remain non-unique: a separate intent may deliberately create the same name.

## Server boundary

`POST /api/areas` accepts an optional `Idempotency-Key`. Existing keyless API
callers retain explicitly non-idempotent creation. The new FE always supplies a
key. It first reads the explicit `areaCreateIdempotency` capability from
`GET /api/version`: missing, false, or malformed means unsupported and no POST
is allowed. A failed capability read also submits nothing and leaves the form
editable. This prevents a new FE from silently trusting an older server that
ignores the header; global startup compatibility remains unchanged. Reuse the shared header parser and versioned canonical payload digest.
The digest covers every typed create input before normalization (`name`,
`color`, `sort`, `default_template_id`, `default_cwd`); omitted and null optional
fields have the existing equivalent semantics. Unknown fields remain ignored.

Add migration 0096 with a permanent binding table keyed by the request key,
carrying the original request fingerprint and minted Area identity. There is
no cascading foreign key: deletion cannot release an old creation identity.
Prevent deletion and mutation of committed bindings with SQLite triggers.
Existing migrations remain byte-frozen. Existing Areas need no backfill because
they have never had a supplied creation identity.

Use the existing `write_with_actor_events_typed` transaction, which acquires the
SQLite immediate writer lock. Check the binding, then atomically insert Area,
defaults, binding, and its one `AreaUpdated` event. A same-key concurrent request
sees the first committed binding. An event/write failure rolls everything back.
The event wrapper refuses empty event batches. A local one-shot channel carries
a proven replay row while its read-only transaction rolls back intentionally;
only that branch can populate the channel, so unrelated errors still propagate.
Replay returns the current row with 201 and emits no event; a mismatched request
or deleted Area returns 409 without minting. Check replay before mutable
workspace/template validation, so a path removed after commit cannot turn a
successful creation into a failed retry. Key identity is scoped to this server's
Area-create endpoint, matching the single-user workspace API's authority.

## Client boundary

The shell owns one Area creation intent: key, color, submitted fields, and
outcome. Allocate color once along with its key; generating color on each click
would change the hidden payload during an otherwise identical retry. Retain an
uncertain submitted intent when the dialog closes and reopen its original
values. Retry the exact submitted payload. Clear identity only after success or
an explicitly discarded intent. Keep the existing compact Name + optional
pills hierarchy; communicate an unknown result with one inline explanation and
one retry action, without adding identifiers to the product interface.

The core operation requires a key and forwards it in `Idempotency-Key`; app
mutation callers carry it explicitly. Update both generated OpenAPI consumers
using the real generators. Legacy callers may stay keyless because that
compatibility is documented, rather than silently invented during this change.

## Acceptance

Exercise the production REST endpoint for repeated/concurrent identical keys,
every changed input, independent same-name creates, deletion replay, malformed
keys, and event-failure rollback. A real frontend request that loses its response
after commit, then retries, must leave exactly one Area and one creation event.
Frontend flow tests pin key/color/payload retention and recovery after closing
and reopening. Mutation verification removes the production replay shortcut in
an exclusive worktree: predict the entire failing test set, run it, safely
restore, and confirm green with no transient changes observed by reviewers.
