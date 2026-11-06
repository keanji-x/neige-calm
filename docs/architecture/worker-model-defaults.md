# Worker model defaults (#3542)

Backend and Settings UI contract. Review tier: L2 (owner authorization
and immutable persisted publication state). No database migration or new
scheduler, identity, cache, or task selection parameter is needed.

## Settings API

The existing deployment-owner `GET/PUT /api/settings` string map owns these keys:

| Key | Worker provider |
| --- | --- |
| `worker_model_defaults_codex` | Codex |
| `worker_model_defaults_claude` | Claude |

Each value is a JSON **string** encoding `WorkerModelDefaults`, for example:

```json
{"settings":{"worker_model_defaults_codex":"{\"model\":\"my-model\",\"effort\":\"high\"}"}}
```

Both inner members are required; either may be null to omit that override at the
provider boundary. Non-null values must be nonempty, without surrounding
whitespace or control characters. Unknown inner fields, duplicate fields, wrong
types and malformed JSON are refused. The outer setting value `null` or `""`
deletes the entire provider pair. GET retains corrupt stored settings so the
owner can inspect and clear them.

The complete patch is validated before any KV write or proxy side effect. Each
pair occupies one KV row; there is no independently updated effort/model row.
The existing catalog checks known unsupported efforts. A missing catalog or an
unlisted slug does not become an allowlist failure: the pair reaches the provider
unchanged. Provider refusal follows the existing visible operation/terminal
failure path, with no alternative model or backend. Existing Planner catalog
policy is unchanged. Updating these keys does not request a daemon respawn.

Errors use the existing JSON error contract: non-owner GET/PUT is 403;
invalid inner configuration is 400 `bad_request` with `field` equal to
`settings.<key>` and a readable reason. Invalid outer request bodies retain the
existing 422 `invalid_body` contract. A corrupt stored setting fails publication
with an internal error naming the key and its repair action, before committing a
worker. A malformed or provider-mismatched card snapshot refuses launch with
409 `conflict`; automatic cold replay logs the refusal and leaves it unloaded.

## Publication snapshot

Both `WorkerAdmission::publish_worker` implementations read the setting through
the supplied publication transaction, ordered against the existing Settings
writer. They persist `worker_model_selection` in the card payload:

```json
{"provider":"codex","selection":{"kind":"explicit","value":{"model":"my-model","effort":"high"}}}
```

Cleared/unset defaults produce
`{"provider":"codex","selection":{"kind":"inherit"}}` (or `claude`). Missing
`worker_model_selection` identifies a pre-upgrade worker, not this new inheritance
state. Exported Rust/OpenAPI/TypeScript types are `WorkerModelProvider`,
`WorkerModelDefaults`, `WorkerModelChoice`, and `WorkerModelSelection`.

The common server-owned payload registry rejects client writes through REST and
plugin callbacks and preserves the field across replacement payloads, including
corrupt values. Defaults changes and clears affect only subsequent publications:
queued tasks sample at publication, and a later attempt gets a new snapshot.
Existing session backend binding remains authoritative and unchanged.

## Launch and recovery

Codex writes the pair to initial thread configuration and the first turn. Its
terminal resume command also receives the snapshot as shell-quoted TOML config
overrides for subsequent turns, using the documented
[Codex CLI config and resume contract](https://developers.openai.com/codex/cli/reference/).
Cold replay lowers
the same card snapshot into thread configuration; hot takeover only restores the
subscription and preserves the loaded thread's settings. Operation re-drive uses
the persisted thread/turn instead of creating a new selection.

Claude's formal terminal command and `claude_restart_adapter` both lower the
same snapshot to individually shell-quoted `--model=value` / `--effort=value`
arguments, using the same provider argument construction as stream-json launches.
No headless-only path or task parameter substitutes for the terminal path.

Legacy workers never read these defaults on launch/recovery. Existing Codex
thread history and Claude session IDs retain their provider resume semantics.
Without a snapshot the kernel cannot prove the exact pre-upgrade model/effort;
legacy recovery emits an explicit warning. This feature does not reconstruct
missing provider history or promise exact legacy selection when provider resume
cannot recover it. Inherit/null members intentionally retain provider semantics,
not a fabricated resolved choice. Only provider reports populate actual model
telemetry; requested defaults/snapshots are never copied into `resolved_model`.

Planner, Assistant, interactive cards and plain terminals do not consume these
worker defaults. Settings → Workers now exposes `/settings/workers` on desktop and mobile, with
independent Codex/Claude drafts, explicit atomic Save/Clear defaults, shared model
catalog suggestions and manual slugs. Platform-independent strict decoding lives
in core/domain; app injects owner-gated query/write capabilities and classified
errors into props-only Settings components. Late responses cannot overwrite or
confirm newer drafts. See the [Settings contract](../../fe/web/src/features/settings/README.md#worker-model-defaults-3542).

The UI implementation does not finish issue acceptance: a real preview, Tier 1,
two independent L2 reviews and PR/CI remain follow-up steps. Real-provider
inference checks are outside these tests; backend regression coverage uses
temporary databases and scripted peers. No schema or generated binding changes
are needed for the UI.
