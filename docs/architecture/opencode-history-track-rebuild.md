# OpenCode history to an ordinary Track

## Outcome and status

A human-created Planner bound to the enabled history builtin, separately granted
Area Admin by the human, selects OpenCode history and asks what the
user wants to rebuild, discovers an existing template or recipe, and creates an
ordinary, non-Admin, top-level Track in its own Area. One Area may claim several
directories; each Track still has one primary workspace. Other directories are
context, not additional workspaces or an implied filesystem access grant.

This is the migration implementation contract. Human REST binding/grant and
native-to-other-owner handoff have an integration regression against upstream
#2783. Real isolated upstream acceptance and two independent implementation
reviews remain separate requirements; synthetic fixtures do not establish those passes.
Review tier **L2**: external private-history access, human authorization and
durable creation cross authority and persistence boundaries. Planner arranges
two independent reviews after this document and registers the implementation
issue required by CONTRIBUTING. Issue #1928 concerns terminal-native sessions;
its permissions do not authorize this feature.

Evidence: audit `runs/e40ac4b8cf934bc7becae361f768f954:opencode-rebuild-contract-audit.json`,
completion event **399479**, checked against checkout
`1bf073e90f157443789b1a163ec2cc405a3374f9`. Revision evidence: first-round review A
`events.completed` **400009** and B **400682** in the original
`runs/e40ac4b8cf934bc7becae361f768f954:opencode-design-review-{a,b}.json` records,
against head `e4a3ba02974f0ca0b460d8bab8a4c8ddeaa075fc`. This revision addresses
A-B1 (configuration), A-N1 (directory listing) and B's existing path limitation;
two fresh full L2 reviews remain Planner-owned. Repository entry points below
were read again. External facts come from that audit's official
[server documentation](https://opencode.ai/docs/server/) and
[v1.19.3 release](https://github.com/anomalyco/opencode/releases/tag/v1.19.3),
commit `e3c4eb242585dcd77be1a867391b7c25fd47ce52`. They do not establish a local
deployment's contract. `command -v opencode` returns no binary; no source has
been configured and no real history has been accessed. Isolated-instance
contract acceptance remains **unperformed**.

## Optional compiled history component (migration contract)

The authorized migration replaces the transitional Python app with exactly one
Rust implementation in `plugin::builtin::opencode_history`. Its typed config,
fixed official protocol, HTTP, identity, parts and evidence budgets belong there.
`calm-server::builtin_plugins::opencode_history` only adapts native registry and
host run access. Reuse `plugin::builtin` declarations and the server catalog.
The component is optional, disabled on first reconciliation, kind `builtin`,
without an entrypoint, process, polling task or Python fallback.
The four native capabilities use the existing agent command vocabulary:
`source_ls`, `session_ls`, `message_ls`, and `message_cat`. No new verb or alias
is introduced. Returned evidence fields and upstream HTTP routes stay unchanged.
Its compiled ID is `opencodehistory`, following the existing one-word builtin
identity contract (#2087). The unreleased transitional app's `history.opencode`
ID is removed, with no compatibility alias. This rename does not change the
fixed upstream session/message/part identities or supported HTTP contract.
Native catalog invariants consume component declarations and the existing full
registry golden rather than a second growing tool-name inventory. Zero-config
role fixtures cover stateless components; components requiring operator config
exercise the same registered entry with their own explicit typed fixtures.


Declare manifest version 4 and `agent_tools_scope: "bound-track"`. Only a Planner
on a Track bound to this owner can read. Enabled unbound Admins, unrelated owners,
Workers, Assistants and disabled components cannot read. Binding grants no Admin or plugin
installation authority. A user creates the `opencode-history-rebuild` template
and grants Admin separately through normal APIs. Upstream #2783 permits explicit
grants to otherwise eligible plugin-bound Tracks. Keep its shared eligibility
policy and released migrations unchanged; test grants through human REST, never
SQL. The bound Planner regression pages/captures native results and creates a
gitforge-bound ordinary Track through catalog/track_add, checking exact first
message provenance, retry uniqueness, scope refusals and live revocation.

The generic host starts one owned builtin run from its effective configuration
for every enable/restart. The run owns an immutable typed snapshot and secrets;
no static configuration or plugin-ID cache can cross host instances. Initialization
loads the operator's scalar `history_config_path`, validates before Running and
publishes Unavailable on refusal. Stop drops the host's run; calls already holding
a run may finish, while subsequent calls must resolve the current lifecycle.
Existing stateless builtins use the same port without behavioral changes.
Blocking file/ureq work runs on `spawn_blocking`, never a Tokio executor thread.
HTTP uses explicit authentication, standard TLS verification, no environment
proxy inheritance, no redirects, a configured total request timeout and bounded
response. ureq documents that blocking system DNS cannot be interrupted and may
exceed that timeout; this is not a hard wall-clock deadline. No resolver thread
pool or cancellation framework is introduced for this migration.

Native and manifest calls share the result recorder, keyed by resolved Track,
registered tool and exact arguments. Successful bounded text must be capturable
by `neige_source_capture`; failures invalidate the key's earlier evidence.
Budget the serialized MCP result including all text escaping and metadata, not
the obsolete Python JSON-RPC request ID. No manual test recording bypass.

The manifest declares only the required scalar `history_config_path`. Host
`config_for_spawn_or_unavailable` supplies effective values; the component checks
file contents. A changed config/password requires restart; no per-call file reads.
The new Markdown template is user-creatable, declares its required owner in the
manifest, retains the work-brief Report contract and has no preactivated tasks.
It guides human Admin grant, explicit history selection/current intent, paged
reads/capture, catalog/template/recipe/cwd selection, exact-key retry and a
self-contained handoff that survives plugin disable. No private history or real
Codex E2E is used for migration verification. Review tier remains L2; formal
independent review is Planner-owned (issue #2777).

The file is UTF-8 JSON, at most 64 KiB, with required typed fields as illustrated:

```json
{
  "sources": [{
    "source_id": "local-history",
    "base_url": "http://127.0.0.1:4096",
    "credential_ref": "local",
    "directories": ["/work/a", "/work/b"]
  }],
  "credentials": {"local": {
    "username": "opencode",
    "password_file": "/etc/neige/secrets/opencode-password"
  }},
  "limits": {
    "page_messages": 50,
    "max_response_bytes": 2097152,
    "max_result_bytes": 131072,
    "max_parts": 128,
    "max_part_bytes": 16384,
    "request_timeout_ms": 10000
  }
}
```

Plugin-local typed decoding rejects unknown fields at every level, duplicate JSON
keys/source IDs/credential refs, missing fields, empty IDs/usernames, unresolved
refs and empty/invalid paths. Require 1..16 sources and 1..64 directories per
source. All file and directory paths are nonempty absolute strings without NUL;
no HOME/XDG, profile discovery or defaults. `base_url` is a fixed HTTP(S) origin without
userinfo, query or fragment. IDs are operator-owned instance labels, not proof
of an OpenCode account/profile. Load passwords only from explicit `password_file`
for referenced credentials: regular UTF-8 files, nonempty and at most 4 KiB,
owned by the current uid with no group/other permissions. Check the opened file's
metadata and bounded contents, not a separate unchecked path probe. Missing,
unreadable or invalid files reject the whole initialization with a sanitized
error category/field, never content. Credentials must not enter logs, results,
captures or handoffs; use them only for Basic authentication to the selected origin.

Every limit is a required integer in the plugin's typed file, with inclusive
ranges: `page_messages` 1..50, `max_response_bytes` 1..2097152,
`max_result_bytes` 1..131072, `max_parts` 1..128, `max_part_bytes` 1..16384,
`request_timeout_ms` 1..10000. These are Planner-selected first-release ceilings;
operators may lower them, never raise them. Each call reads at most one explicit
page (or one selected message), with no hidden traversal or page-count setting.
Planner decides whether to request a continuation. Bound list/message limits by
`page_messages`. Enforce the actual serialized UTF-8 result budget after retaining
parts and adding scope, coverage, omission markers and digest, including JSON
escaping/metadata overhead. `max_parts` caps retained parts across the result;
`max_part_bytes` bounds each retained serialized part. Page and single-message
outputs share the total result budget; per-part limits do not replace it.
If even a truthful omission envelope cannot fit, return an explicit budget
failure without successful evidence or a
capture claim. No unbounded/default-all mode or new retention engine is added.
Emit the bounded evidence envelope as MCP text so existing result capture can
retain it; `structuredContent` alone is insufficient.

Only fixed-origin GET routes below are allowed. Reject redirects, including
same-origin redirects, before forwarding credentials. Reject arbitrary URLs,
hosts, HTTP methods and extra query fields. Encode IDs as single path segments;
never follow attachment URLs. Each session/message read must recheck that the
returned project and directory match the selected allowlisted scope. Refuse
unknown identity, unavailable source, auth failure, version/schema mismatch and
oversized response explicitly; none becomes an empty successful result.

| Official v1.19.3 request | Meaning and restrictions |
| --- | --- |
| `GET /global/health`, `GET /doc` | Require version `1.19.3` and validate the used running OpenAPI contract. Reject another version; upgrades require adapter contract tests, not a permissive fallback. `/doc` alone does not prove behavioral compatibility. |
| `GET /project`, `GET /project/current?directory=D` | Discover reported `Project.id/worktree/sandboxes`; validate selection. A project is not an Area or an operator source ID. |
| `GET /session?directory=D&roots=R&search=S&limit=N` | Directory-filtered, updated-descending bounded list; omit `scope`, never send `scope=project`. `search` is title SQL LIKE, not body search. Return scope and possible truncation. No stable backward list cursor; `start` means updated-at-or-after, not next page. |
| `GET /session/{sessionID}?directory=D` | Validate reported session `id/projectID/directory/path/parentID/version`; do not infer identity from storage or git hashes. |
| `GET /session/{sessionID}/message?directory=D&limit=N&before=C` | Require positive limit; omit `before` on first page. Follow server `X-Next-Cursor` or `Link rel=next` opaque cursor, never construct one. Validate Link origin/path/query and extract the cursor rather than requesting an arbitrary Link URL. Invalid cursor is an error. |
| `GET /session/{sessionID}/message/{messageID}?directory=D` | Explicit selected-message read returning `{info,parts}`; no claim of server-side part pagination. |

The external source anchors are OpenCode
`packages/opencode/src/server/routes/instance/httpapi/{groups,handlers}/session.ts`,
`packages/opencode/src/session/{session,message-v2}.ts` and
`packages/sdk/js/src/v2/gen/types.gen.ts` at the pinned commit above.
At the pinned commit, `groups/session.ts::ListQuery` makes `scope` optional;
`handlers/session.ts::list` passes the resolved directory unless `scope=project`.
`session/session.ts::listByProject` applies project ID and directory equality
before SQL `limit` when `scope` and `path` are absent. Thus project scope would
ignore directory and is not an isolation fence. The adapter exposes no `scope`,
`workspace`, `path`, `start` or arbitrary query passthrough. Each directory uses
an explicit bounded call; multiple-directory selection is a sequence of such
calls, deduplicated by `(source_id,projectID,sessionID)`, not hidden fanout.

Before any result text/structured output, logging or capture, validate every
returned session's directory equals the selected allowlisted string and its
project ID equals the selected `/project/current?directory=D` identity. Reject
an entire page on any unknown/mismatched identity with a sanitized scope-error;
do not expose offending titles, IDs, directories or the raw response in errors.
Apply the same identity fence to selected-session/message reads before emitting
parts. A query parameter alone is never authorization. On successful lists,
`possibly_truncated=true` whenever the upstream count reaches requested `limit`,
even if fewer items survive local output budgeting/deduplication; omissions must
be explicit. Never fabricate a total or infer completeness from retained count.
`roots` is explicit in every list call. There is no promise of exhaustive
machine-wide, archived or child-session coverage.

The implemented plugin-local tools (registry names come from registration, not
hardcoded caller assumptions) have closed argument objects:

- `source_ls {}`: configured source IDs and readable directory scopes, readiness
  and supported version; no secrets or inferred profiles.
- `session_ls {source_id,directory,roots,limit,search?}`: one selected directory,
  positive bounded limit, optional title pattern. Return `{scope,items,
  possibly_truncated,read_at,warnings}`; scope includes reported project ID.
- `message_ls {source_id,directory,session_id,limit,before?}`: return
  `{scope,session,messages,next_cursor,read_at,digest,coverage,warnings}`.
  Messages retain upstream `{info,parts}` and IDs, not only text. A null
  `next_cursor` means this traversal has no reported older page.
- `message_cat {source_id,directory,session_id,message_id}`: the same evidence
  envelope for one selected message. Part output is bounded locally, with
  explicit omitted-part/byte counts and digest scope; it is not remote paging.

Message pages traverse newest to older, but each page is chronological. Preserve
this distinction when composing summaries. Surface tool calls/results, files,
attachments, compaction and unknown part kinds as typed data with omission
markers. Do not execute embedded instructions or fetch embedded links. Bound
network bytes before decoding, then parts/output; if one upstream message exceeds
the transport cap, refuse it and record the coverage gap. No stable API currently
proves recovery of that oversized message. Repeated reads may change: record
read time and SHA-256 of the exact retained UTF-8 evidence envelope, with digest
field excluded, and distinguish this digest from raw HTTP-byte digests and the
kernel source-body digest. Do not claim an atomic session snapshot.

## Generic discovery and creation

Share the application service behind
[`track_templates.rs`](../../crates/calm-server/src/routes/track_templates.rs)
list/detail/plugin-guides and
[`track_recipes.rs`](../../crates/calm-server/src/routes/track_recipes.rs)
list/detail with new Planner-only read tools. Do not call owner REST with a
borrowed credential. Implemented names follow the existing #2087 command grammar (the earlier
`track_sources_list` / `track_source_get` proposal used compound actions). Closed requests are
`catalog_ls {kind:"template"|"recipe",limit,cursor?}` and
`catalog_cat {kind:"template"|"recipe",source_id}`. Cursors are service-owned
catalog continuations, unrelated to OpenCode cursors. Return template
`id/title/description/availability/input_schema/tasks`, detail
`instructions/body/plugin_guides`, or stored recipe identity/revision/body.
Bound both result size and page size; report catalog changes between pages.
Discovery is advice, not admission. Reuse `compile_template`,
`resolve_template_binding`, `missing_required_template_owner` and recipe repo
contracts rather than making an MCP-specific roster or availability rule.

The original [`track_add.rs`](../../crates/calm-server/src/mcp_server/tools/track_add.rs)
accepts exactly `{recipe_id,title,idempotency_key,text,message}`. All five are
required; title is at most 200 characters, key uses the shared 128-byte ceiling
and forbids whitespace/control, text uses `validate_first_message`, and message
uses shared write-note validation. Keep that request and its serialized
five-field fingerprint byte-compatible. Keep existing success
`{track_id,created_at}` and refusal vocabulary.

The implemented extended request uses the same top-level common fields
`title,idempotency_key,text,message`, exactly one of `recipe_id` or `template_id`,
optional `template_input` only for its bound template, and optional `cwd`,
`attach_folder`, `allow_cross_area_cwd`. The latter, when permitted, is exactly
`{folder_id:integer,area_id:string}`. Reject unknown keys, null selections,
blank IDs, simultaneous selections and input without a template. An explicit
`cwd` must be absolute; omission retains the old managed-default behavior.
Reject attach/authorization without explicit cwd. No `area_id`, provider/model,
arbitrary `plugin_scope`, fork, Admin or multi-workspace selector is added.

Select legacy hashing only when the original five-field wire shape is present.
For all extended shapes hash a versioned canonical object containing every
supplied selection, template input, cwd, attach/authorization and common request
fields; absence must not collapse into false/null. Derived caller,
Area/provider and mutable catalog advice remain outside the fingerprint.
Thus old retries retain their binding, while a changed new selection conflicts.
History evidence and intent are already bound through the exact `text` bytes;
do not add an OpenCode-specific creation field.

[`routes/tracks/add.rs`](../../crates/calm-server/src/routes/tracks/add.rs)
originally forced `default_cwd`, `TrackInit::Recipe`, `FolderClaim::Skip` and
`ManagedUnder`. Extract shared named-source/workspace preparation from
[`routes/tracks.rs`](../../crates/calm-server/src/routes/tracks.rs):
`NamedSource::resolve` -> `CreationSource::stamp` and
`validate_template_input_binding`, attached-workspace/git-root validation and
`FolderClaim::Enforce`. Keep replay selection before mutable mint preparation.
Both REST and MCP use this service; do not duplicate validation in the handler.
Then retain `plan_keyed_create` -> mint `create_track_with_first_message` or
`resume_prior_attempt` -> `create_track_structure`, materialization and fenced
[`first-message delivery`](../../crates/calm-server/src/routes/tracks/create/delivery.rs).

Identity comes from authenticated `ToolCallIdentity`; Area comes from the caller
Track and provider from `track_planner_provider`. Preserve registry Planner-only
gating on mint and replay, transaction `CreatorAdmission::admit_tx` open-Admin
check and max-open cap, creator stamping and `(area_id,key)` binding in the same
transaction. Namespace remains `track-add/{creator_track_id}/{key}`. Replay
does not rerun mint admission or cap accounting; preserve that existing contract
rather than asserting that catalog filtering authorizes a call.

Template admission is user-creatable roster membership, with running/trusted
owner resolution. `tracks.plugin_scope` records the single owner, resolved by
`track_binding::resolve_track_owner_binding`; recipe creates remain unbound.
`mcp_server/tool_visibility.rs` and `transport::dispatch_plugin_tools_call`
enforce the same scope on list and call: unbound union subject to
`agent_tools_scope`, owned only its owner, unavailable owner no plugin tools.
Guides/instructions do not enable tools. The history builtin need not own the target
template. Install/enable/config remain human deployment-owner operations under
`routes/plugins.rs`, not Area Admin privileges.

## Directories, authorization and retries

Use `routes/area_folders.rs` resolution and
`tracks::enforce_folder_claim_tx`, backed by the existing atomic string-path
overlap checks in `calm_truth::area_folder_claim`. Its `normalize_path` removes
exactly one trailing slash; it does not resolve `..`, collapse repeated slashes
or resolve symlinks. Existing #1147 N19 tests demonstrate missed overlaps for
`/a//` versus `/a/b`. Neither these claims nor history directory equality prove
filesystem alias isolation. Use explicit operator-selected path strings; do not
claim all spellings of one location are equivalent or isolated. Fixing #1147 or
adding canonicalization is outside this design.

Same-Area covering claims need no new attachment; unclaimed cwd requires explicit
`attach_folder:true`. Never auto-create an Area
from an OpenCode directory. An attached repository produces a Track worktree;
verify actual agent cwd rather than equating it to the historical directory.

Cross-Area reuse is confined to the exact current `{folder_id,area_id}` and
selected cwd, leaving the claim with its original Area. Deleted/replaced claims
and ancestor overlaps refuse atomically; a stale authorization must never
degrade into attaching an unclaimed path. **First-release restriction:**
`user_ask.rs` only has typed `reopen_track` and `grant_admin` actions. There is
no inspected MCP human-confirmation receipt binding a conflicting claim/cwd to
this creation. Merely letting an AI send the REST-shaped pair is insufficient.
The first release therefore always returns a clear unsupported-confirmation
refusal for cross-Area MCP creation; no new authorization action is added.
Free-text approval, a write audit note and Admin status cannot impersonate a
grant. Future enablement needs a separately designed verifiable human binding;
same-Area flows can proceed. Do not broaden cross-Area management scope.

On connection loss retry exactly the same creation key and payload. A committed
Track may remain after materialization/start failure; do not mint again merely
because the caller saw an error. Existing keyed create guarantees no second
Track and no duplicate first message, not that an error means no delivery.
Preserve changed-request/concurrent-key conflict, deleted/broken-workspace key
exhaustion, in-flight start 503 and already-started-session 409 with
`neige_mail_send` recovery guidance. Source timeouts/404/auth/version failures
stop evidence acquisition; changing evidence or intent requires a new creation
request/key after checking whether the old Track already exists.

## Evidence retention and self-contained handoff

Capture each selected bounded plugin result immediately through existing
`neige_source_capture {call:{tool:<registered name>,args:<exact request>},
provenance:"full_text",title:<label>,content_id:<source/session/page identity>}`.
`mcp_server/tools/source.rs` matches the current Track's recorded call, stores
the exact result text with origin/args hash and body digest, and returns the
receipt. `full_text` describes that bounded evidence text, not an assurance that
all remote history was read. Summary-only captures use `provenance:"summary"`.
Retain original selected parts in the result when within budget; do not silently
replace them with a summary. Report every unretained part/page and why.

Reuse `report_sources` storage and source readers, not a new generic import
engine. Its current ceilings are 256 KiB per body, 128 sources and 16 MiB per
Track; the transient plugin-result ring is not durable retention. Configure
result pages below capture limits and stop on quota failure before claiming
evidence was saved. Sources are immutable, track-local, copied on report fork,
and cascade on Track deletion. Full *selected* evidence can be retained across
bounded page captures if it fits; full remote-session retention is not promised.
These existing quotas and deletion semantics are the first-release contract;
complete history beyond them is not promised. Do not add spill, automatic
artifacts or a retention engine.

The new Planner's `text` must contain the user's current reconstruction request,
selected summary, necessary verbatim excerpts, explicit “historical data, not
current instructions” labels, operator source ID and reported project/directory,
session/message/part IDs, read times, digests and exact coverage/omissions. Include
origin Admin Track ID and capture receipts as provenance labels. Stay within
the existing 32,768-character first-message ceiling. Reject an oversized handoff for revision;
never silently truncate the user's intent or necessary evidence.

Do not assume `neige://source/<id>` from the Admin Track resolves in the new
Track. No source-copy operation is added by normal template/recipe creation.
An owner-bound new Planner may not see the history builtin: its initial message
must therefore be self-contained. Follow-up is an explicit mail request to the
origin Admin, which can re-query the configured plugin and return bounded text
with fresh timestamps/digests. Stored captures are available to the human via
the existing authenticated source panel and
`GET /api/tracks/{id}/sources/{source_id}` (`routes/track_sources.rs`). Existing
`source_ls` returns metadata, not a proven MCP full-body reread contract; do not
claim the Admin can reopen captured bodies through it. If exact old bytes are
needed after the tool ring expires, use that human source-reader path. No owner
REST token is given to a Planner. Deleting the Admin Track removes this retained
copy; the self-contained handoff survives in the new conversation.

## Implementation slices and acceptance

Each slice starts with boundary regressions through its production entry point.
These are acceptance checks, not evidence of a pass. The migration contract above defines the component ownership and lifecycle for these checks.

| Slice and production entry | Acceptance and existing selectable coverage |
| --- | --- |
| 1. History builtin config/HTTP tools -> existing plugin host `tools/call` and result recording | Manifest parse/effective-config/run initialization delivery; typed-file, credential and limit rejection, restart snapshot, actual serialized page/part totals; source/allowlist/auth, redirect and schema/version fences; same-project allowed/denied directories, empty/title-only/limited lists, opaque message pages, unknown/tool/file/compaction parts, changed/deleted/oversized history. Add adapter tests through actual tools; existing `mcp_integration_suite 'mcp_plugin_tools::'`, `plugin_suite 'plugin_config_delivery::'` and `'plugin_management_auth::'` cover integration boundaries, not the migrated adapter. |
| 2. Shared discovery service -> new Planner read tools and existing REST picker | Identical template schema/availability/detail and recipe revision, bounded continuation, stopped/untrusted owner and Worker refusal; `track_suite 'track_templates_read::'`, `'track_recipe_instantiate::'`, `'plugin_scope::'`. |
| 3. Shared preparation -> `RouteTrackCreator::add` and REST create | Legacy five-field fingerprint/replay unchanged; every new field bound, template input validated by owner, ordinary Track, Admin revoke/cap/provider failure; `track_suite 'track_add::'` and `'track_create_first_message::'`. |
| 4. Explicit workspace -> `enforce_folder_claim_tx` and materialize | Two same-Area nonoverlapping directories, explicit attach, agent worktree cwd, concurrent/ancestor conflict rollback, stale exact cross-Area claim; `track_suite 'track_cwd_terminal_at::'`, `domain_api_suite 'area_folders::'`. First-release cross-Area MCP must refuse without side effects. |
| 5. Admin selection -> source capture -> `text` -> fenced harness start | New Planner sees intent/excerpts/IDs/coverage even under a different owner; multi-page capture and quota errors, interrupted create/retry yields one Track and one first message; `track_suite 'track_add::'` and `'track_create_first_message::'` plus new end-to-end handoff tests. |

Run a selected existing filter with, for example:

```sh
env -u NEIGE_CODEX_BIN RUSTC_WRAPPER= CARGO_BUILD_JOBS=6 \
  cargo nextest run --locked -p calm-server --test track_suite \
  'track_add::' --test-threads 8
```

The table's suites/filters are registered in current test roots; reject an empty
selection. Critical existing assertions include
`track_add_admin_replays_and_refuses_changed_request`,
`track_add_admin_delivers_text_once`, `track_add_after_revoke_commits_nothing`,
`every_mint_input_is_bound_to_the_track_create_key`,
`post_api_tracks_rejects_stale_cross_area_cwd_authorization` and
`cross_area_cwd_authorization_rejects_deleted_claim_without_attaching`.
Mutation-verify load-bearing authorization/idempotency fences with predicted
complete red sets in an exclusive recoverable worktree during implementation.

Configuration acceptance uses the real manifest parser, effective-config and builtin run
initialization entry points: the example schema is accepted; missing required path,
relative/empty paths, unknown/duplicate fields, unresolved refs, unreadable or
wrong-mode/owner/oversized passwords and limits outside each inclusive range
refuse readiness. Replacing JSON/password while running changes no source or
credential until restart. Test escaped text/metadata/omission overhead against
actual serialized bytes, including settings too small for a valid envelope.

Directory acceptance seeds two sessions in the same project: `/work/allowed`
is configured and `/work/denied` is not. Assert outbound list query has explicit
`directory`, `roots`, `limit`, optional `search`, and no `scope/path/workspace`.
Normal responses return only allowed IDs; inject an upstream mixed/unknown page
and require whole-page refusal with no denied titles/IDs in result, logs or
capturable text. A list at upstream limit remains possibly truncated after local
omission/deduplication. Keep claim tests within their documented string contract;
do not claim these fixtures fix #1147 or symlink aliases.

Normal scenario: the operator configures one isolated source with repositories
`/work/a` and `/work/b`, both claimed by the caller's Area. Admin lists bounded
title matches in both scopes, reads selected pages and asks the user to rebuild
only the migration work. It discovers a ready template, captures evidence,
chooses `/work/b` as cwd and creates once. The target Planner receives the
self-contained migration handoff and runs in its materialized Track worktree.

Conflict/retry scenario: `/work/b` is covered by another Area. The initial call
refuses without creating or attaching anything. The first-release cross-Area
MCP path stays refused. Existing exact-claim REST authorization must still reject
changed claims. For a valid same-Area creation, disconnect after commit and retry unchanged:
observe one Track/one first message, or the existing explicit start-recovery
refusal. A new key is not an automatic network-error recovery strategy.

Actual external acceptance requires an operator-selected isolated v1.19.3 HTTP
instance containing only synthetic sessions, with explicit source credentials
and directories. Record `/global/health` and `/doc`, then exercise the production
plugin tools against that server, including same-project multiple directories,
different projects, child sessions, long pages, malformed cursors, large parts,
auth failures and a controlled redirect endpoint. Compare returned IDs, part
coverage and cursor headers with the official responses; fixtures alone do not
prove the running contract. Provision synthetic history outside the read-only
plugin. No actual instance acceptance has run here; the missing binary/source
configuration must be supplied before claiming external compatibility.

## Change surface and architecture checks

Sweep `TrackAddArgs` parser/schema/fingerprint/tests, its tool prompt and registry
goldens, CLI schema-derived help/call fixtures, shared named-source/workspace
preparation and all `create_track_structure` callers: message-less/keyed REST,
keyed add and `create_managed_track` (daily planner). Preserve the child-only
`operation/child_track_adapter.rs::prepare_tx` lifecycle rather than routing
rebuilds through child creation. Update discovery registration/prompts/goldens
and adapter manifest/config contracts. If public REST/types change, sweep
`fe/core/domain/track.ts::createTrackOperation` and
`fe/web/src/app/providers/queries.ts`, regenerate through `(cd fe && npm run gen:api)`
and include affected `fe/core/api/generated/openapi.json`, `wire.ts` and generated
type bindings. Do not hand-edit generated contracts or churn them without a
schema change; external REST clients cannot be exhaustively enumerated.

- **Abstraction boundaries:** builtin owns OpenCode semantics; application service
  owns discovery/preparation; kernel retains generic identity/lifecycle and
  transactional authorization. First-release cross-Area refusal and bounded
  retention are intentional scope limits, not pending routine-budget approvals.
- **Duplicate logic:** share template/recipe, owner, claim, keyed create,
  materialization and first-message entry points. Do not introduce parallel
  roster, claim policy, importer or conversation replay machinery.
- **Hardcoded application assumptions:** no OpenCode branch, plugin ID or new
  multiple-owner policy in generic layers; use registrations/manifests. Fixed
  upstream route names/version belong only in the adapter's supported contract.
- **External contract facts:** use reported identities and opaque cursors;
  operator source declarations do not prove private storage/profile mappings.
  No private-layout derivation or compatibility fallback.
- **Cost of permanence:** this document's test list is planning evidence, not a
  permanent CI allowlist. An ordinary test addition edits its owning test module
  and existing registration only when needed; no second name inventory. A
  version upgrade edits adapter contract/config and its boundary tests, not
  kernel policy. A source/directory/credential change edits operator JSON (and
  the referenced password file when needed), then restarts the plugin; no
  manifest/kernel roster changes. A new typed field or limit changes the plugin
  decoder/validation and boundary tests, not the generic schema validator.
  A new history tool edits its component schema/declaration and prompt, then
  regenerates the existing registry golden; native binding follows declarations.
  The pre-existing Assistant authority matrix independently adjudicates public
  tool permissions, so additions/renames update that existing expectation too.
  Bootstrap uses the same explicit operator fixture as the history integration;
  it does not backfill a missing required configuration.
  The existing aggregate Planner surface cap accounts for the four new native
  tools (+1,740 declared bytes); there are no new per-tool caps or test-name pins.
  Add only lasting boundary assertions, using existing gate selection.
  Implementation gates run text/contract/quick once plus one nonempty
  focused replay per candidate; CI owns broad suites.

Migration verification: worker runs only the four frozen focused commands,
with red/green and exclusive single-factor identity/scope mutation evidence.
The kernel owns text, contract, quick, focused replay and diff gates. Actual
isolated upstream-instance acceptance must be reported separately from the
human REST Admin integration and synthetic component/fixture evidence; no
external pass is implied.
