# UX cycle 2: manage the current portfolio from a new conversation

Status: independent reviews, tests, and candidate UI acceptance passed on 2026-09-09.
Issue: https://github.com/keanji-x/neige-calm/issues/1600
Previous closed loop: table display, integrated256145ae4 and used on5200.

Outcome: in a new Assistant conversation on a test Track, the user can record
one explicitly fictitious security quantity, read its quote/current holdings,
and see the matching current-Track market overlay. No need to switch to Planner.
This cycle does not change table layouts, cash accounting, price precision,
transaction semantics or template editing authority.

Contract: add optional `assistant_access: bool` on ExposedTool, default false.
An Assistant may call a tool only when it is explicitly opted in, belongs to a
installed, enabled/running local App plugin with explicit per-tool opt-in, has no execution-backed `kind`, carries a
resolved nonempty current Track identity, and passes the existing TrackPluginScope.
Keep legacy Planner/Worker behavior. Reject opt-in on unsupported manifest kinds.
Remote HTTP/CLI materializers explicitly set false; upstream metadata cannot
self-grant access. Discovery and dispatch share the eligibility decision; shared
unbound daemon discovery may retain its current union behavior, with strict
identity checks at dispatch.

Enable the flag only on market.quote, market.holdings.list and
market.holdings.set. The current Track is injected through the existing kernel
_meta channel, never selected by model-supplied arguments. This is not a new
per-call sandbox for arbitrary local plugin internals: local plugin callbacks
remain plugin-permission-scoped. Market already uses only injected Track identity.

Acceptance: actual Assistant dispatch/query/set/read-back on its own Track;
default-deny legacy tools; denied missing/cross-session identity and wrong plugin
scope; denied stopped plugins and execution-backed/remote opt-in; malicious
argument Track IDs do not change the target. No real brokerage order capability.
Meaningful negative assertions must be mutation-verified. Run focused Rust gates,
then two independent Agent tests/reviews on a fixed snapshot. Integrate only this
closed loop after both pass; update only the independent trial service and test
through its normal new-conversation UI using fictitious records.


Implementation keeps the existing authenticated socket and Track metadata path.
`plugin_tool_roles` determines the exposed role set for both discovery and
routing; `plugin_role_has_track` requires nonempty resolved Track identity for
Assistant on both paths. The shared daemon's unbound discovery union remains
unchanged. Local plugins still operate within their existing installation and
callback permissions; this flag does not add a sandbox around plugin internals.

The optional manifest field fails closed on old kernels: they ignore the new
field and retain their existing Planner/Worker-only dispatch. Existing manifests
on the new kernel deserialize it as false. No stored Report, recipe, DB schema,
or generated API contract changes. The server test fixture reuses its existing
host/session setup; new assertions live in small separate modules rather than
expanding the existing large test bodies.


Validation before independent source review:

- A real Assistant socket call first failed with `-32602`, requiring
  `[Planner, Worker]`; the manifest regressions failed on all three new rules.
- A private Cargo target rebuilt current workspace crates after another task's
  shared-cache build caused stale-artifact compile failures. Those compile
  failures are not counted as mutation evidence.
- 131 focused tests passed: manifest/connector validation, shared role eligibility,
  existing plugin dispatch, and Assistant dispatch against the real market process.
  The market process used only the existing local Sina fixture: quantity 7,
  quote 1316.94 CNY, exact caller KV/overlay, and no forged-target write.
- On the final identity-first code, the 13 role/dispatch tests passed. Removing
  only the explicit opt-in check produced exactly the predicted three failures:
  `ordinary_local_tools_require_explicit_assistant_access`,
  `assistant_token_cannot_call_a_plugin_tool`, and
  `assistant_opt_in_discovers_and_dispatches_with_injected_track`.
  Restoring the original file returned all 13 to green.
- Bypassing only manifest opt-in type validation produced exactly the predicted
  `assistant_access_rejects_execution_backed_tools` and
  `assistant_access_rejects_http_and_cli_connectors` failures. Restoring the
  original file returned all three manifest tests to green. The script compared
  complete predicted/actual failure sets and restored source bytes after each run.

At that source-review checkpoint, quick Rust preflight and immutable artifacts
were still pending. The implementation agent did not modify a running trial
service or invoke a real model.


Independent review fixes (second source checkpoint):

- Duplicate exposed names were accepted, allowing discovery to advertise an
  opted-in second declaration while dispatch chose the first denied declaration.
  A real parsed-manifest/socket test reproduced the mismatch. Manifest validation
  now rejects duplicate names regardless of grant order; the two shipped plugin
  manifests (12 declarations) contain no duplicates.
- Review A supplied a deterministic real-socket/public-reload reproduction using
  the existing scope-resolution tracing event as a bounded observation barrier.
  It proved that an old opt-in could execute a newly reloaded denied App client.
  Authorization now uses `tool_call_snapshot`: the existing lifecycle guard
  captures the tool declaration, connector kind and concrete client together;
  the transport calls only that captured client. It does not keep the guard
  across the RPC, and it cannot look up a replacement client after authorization.
  Kind/client mismatch fails closed. This changes no persistent schema.
- Both reproductions are green in the updated 140-test focused set, including
  all transport unit tests. The first checkpoint's quick Rust preflight was
  fully green, but compile/lint/runtime artifacts must be refreshed for these
  fixes. New source requires fresh independent reviews before integration.

Final acceptance of `7d4d723a1`:

- Both independent reviewers accepted the complete updated diff and separately
  ran the immutable test archive: 140/140 passed in each isolated environment.
  Archive SHA256: `368af77f1fdd6ce6aa0e5a8db766aa7cfcf1f716044f5688db291722ba631558`.
- Updated opt-in mutation produced exactly four predicted failures, including
  the reload regression, then restored 15/15 green. Removing duplicate-name
  validation produced exactly two predicted failures, then restored 2/2 green.
  Original source bytes were restored after each mutation.
- Final `scripts/local-rust-gates.sh --quick` passed formatting, lint, default
  library/release builds, and both OpenAPI drift checks. The default-feature
  Market release build also passed. No real Codex E2E suite was enabled or run.
- The primary agent used a fresh independent candidate service and the normal
  New conversation UI to ask an Assistant to record a fictional SH:510300
  holding of seven units, then query holdings and quote. All three real Market
  tool calls returned successful structured results. The own-Track overlay
  contained exactly that holding; the five-column table displayed its price
  and 100.0% weight after a browser reload. The complete Report payload was
  unchanged, including the empty transaction log. No brokerage order was placed.
- Live quote precision remains a separate known issue: the quote was 4.637 CNY,
  while the existing holdings/table presentation rounded it to 4.64 CNY. This
  cycle preserves the existing presentation and does not claim to fix precision.
