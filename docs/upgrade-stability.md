# Upgrade Stability — Tiers, Surfaces, and Review Discipline

**Status:** policy doc; the rules below apply to every PR going forward.
**Scope:** every persisted or cross-process surface in neige-calm — SQLite store, WS/REST wire, MCP plugin boundary, terminal daemon framing, frontend cache.
**Audience:** anyone opening a PR that touches one of those surfaces.

## Purpose

neige-calm is pre-production. No external installations, no enterprise SLAs, no contractual backward-compat windows. That sounds like a license to break things at will, but it also means we have no external pressure to keep us honest about *which* breakages we mean and which ones we just inflicted on ourselves by accident.

This document defines four upgrade-stability tiers, classifies every surface in the system into one of them, and lays down the rules each tier imposes on a PR that touches it. The goal is not "no breaking changes." The goal is **breaking changes that are intentional, classified, and visible in code review** — not silent ones that surface days later when somebody's binary refuses to boot against their on-disk DB.

Non-goals (repeated at the end):

- **Not** implementing production-grade backward compat. A breaking change at a Tier B surface is allowed; what's not allowed is shipping it without bumping the version and updating the handshake.
- **Not** designing terminal protocol v2 here — that is issue #44.
- **Not** trying to make Tier C and Tier D surfaces stable. The doc explicitly forbids accidentally signalling stability for things that aren't.

## The four tiers

### Tier A — Persistence contracts (must be migratable)

Anything that lives on disk after the server process exits. Breaking a Tier A surface means losing user state. Even in single-user local-host mode, that's the worst failure mode in the system — there is no "just restart the server" recovery.

The rule is **forward-only, migratable, refuse-on-unknown**. The new binary must read the old DB. The old binary, asked to read a newer DB it doesn't recognize, must refuse to boot with a clear message — not silently corrupt the schema by running stale code against rows it doesn't understand.

Tier A surfaces in neige-calm today:

- **DB schema.** Managed by `sqlx::migrate!`. Migrations live under `crates/calm-server/migrations/` and run forward-only on startup (`crates/calm-server/src/db/sqlite.rs:68`).
- **Sync event log payload.** Every row in the `events` table is part of the audit + replay contract from `docs/sync-engine-design.md`. The envelope-level `eventVersion` field tells future binaries which schema applied when the event was written.
- **Kernel-owned card payloads.** `Card.payload` is `serde_json::Value` (`crates/calm-server/src/model.rs:91`), but for kernel-owned card kinds the payload must carry a per-kind `schemaVersion` and have a local migrator helper so older rows can be lifted to the current shape on read.
- **Plugin manifest.** `manifest_version` is a hard gate — wrong version, plugin refused. `min_kernel_version` is honored via semver compare; a plugin requiring a newer kernel than the running binary is refused at load.

### Tier B — Cross-process negotiation contracts (must handshake + fail explicitly)

Anything that crosses a process boundary while both sides are running. The components in this layer — kernel, plugin processes, terminal daemons, web frontend — deploy independently in time. A user might restart the kernel without restarting their open browser tab. A plugin process might have been compiled against an older kernel manifest. **Silent incompatibility here is the worst failure mode**: the system appears to work, then misbehaves under a specific event ordering.

The rule is **negotiate at handshake, fail loudly on mismatch**. Every Tier B surface carries an explicit version. The receiving side compares versions on first exchange and refuses to proceed on mismatch.

Tier B surfaces in neige-calm today:

- **MCP `protocolVersion`.** The kernel sends `KERNEL_PROTOCOL_VERSION = "2025-11-25"` on init; the plugin echoes back its supported version. Today this is sent but not compared — the comparison is what this tier requires.
- **MCP capability versions.** Inside `experimental.dev.neige/*` (and any other capability namespace), each capability carries a `version` field. Today we check presence only (`mcp.rs:450`); the rule is to compare `version` and treat a mismatch as capability-absent (with a warn log).
- **Terminal daemon framing.** The `calm-session-daemon` IPC carries bincode frames over a unix socket. Today the framing is length-prefixed only (`crates/calm-session/src/lib.rs:87`) — no magic, no version. The rule is `magic + version` in the frame header; on mismatch the kernel kills and respawns the daemon (or marks it `needs_restart`).
- **REST API.** Today OpenAPI advertises `info.version = CARGO_PKG_VERSION`, conflating the API contract with the binary release. The rule is an independent `apiVersion` constant, bumped only when the wire shape changes. Old clients get a "please refresh" response.
- **WebSocket envelope.** Today's frame is `{ "_id": ..., "ev": ..., "data": ... }` — no version. The rule adds `eventVersion`; an old frontend on a newer server is told to refresh.
- **Frontend ↔ backend skew.** Today the cache buster is `PERSIST_BUSTER` at `web/src/api/persistConfig.ts:54`. The rule is to surface a `minWebBuildId` in `/api/version`; a frontend below the minimum is force-refreshed.

### Tier C — Internal contracts (no version, no guarantee)

Anything that lives entirely inside a single process and is not visible across a process or persistence boundary. Refactoring a trait, splitting a module, renaming a private function, restructuring a React component — all Tier C.

The rule for Tier C is the opposite of the others: **do not version it.** Adding a `version` field to an internal contract is actively harmful — it signals stability to callers that should never have been depending on it. A `version: 1` on an internal trait reads, to a future contributor or to an LLM scanning the code, like a promise. It is not a promise. We don't make promises about internals.

Tier C surfaces in neige-calm today:

- **Repo trait internals.** Recently split into `RepoRead`, `RepoEventWrite`, `RepoSyncDomainRaw`, `RepoOutOfDomain` (see §1.4.1 of the sync-engine doc). The split is internal architecture — the only stable thing is the *behavior* visible through Tier A (persisted writes) and Tier B (wire shape).
- **Route handler implementation details.** A handler can be rewritten, split, merged, or extracted without bumping anything. What's stable is the REST API (Tier B), not the function that serves it.
- **React component structure and props.** A component can be renamed, split, replaced with a different library, or eliminated. Stability lives at the WS envelope and REST API (Tier B), not the component tree.
- **Sync engine internal buffering and snapshot mechanics.** The subscribe-first-then-query mechanism, broadcast channel sizing, snapshot fallback heuristics — all internal. The wire-level contract (`_replay_complete` / `_snapshot_required` frames, the `since` cursor protocol) is Tier B; how we implement it is Tier C.

### Tier D — Experimental / observable-only (explicit "may break" marker)

A small special-case tier for things that aren't stable, but differently from Tier C. Tier C is "internal — nobody outside should ever touch this." Tier D is "exposed to be looked at and prodded, but absolutely not relied on." Two reasons we have a Tier D surface today: (a) a parser that hasn't seen its first real-world stress test, or (b) an ABI on a deprecation path toward a planned replacement (the MCP-Apps migration in `docs/m3-mcp-apps-migration.md`).

The rule is **an explicit `EXPERIMENTAL` comment header** — a literal `// EXPERIMENTAL: may break without notice` near the top of the file or relevant function. Tier D surfaces are **not** surfaced through `/api/version` or any other negotiation path; they don't get a `version` field, because we are explicitly disclaiming the contract.

Tier D surfaces in neige-calm today:

- **TUI screen adapters.** Observable infrastructure, not a stable surface. Anyone scripting against it does so at their own risk.
- **Claude / Codex semantic parsers.** Heuristic extractors that pull structure out of model output. The underlying model output isn't stable either, so the parser can't be.
- **Plugin card rendering ABI.** Pre-MCP-Apps. Once we migrate to `ui://` resource URIs and the AppBridge model (see `docs/m3-mcp-apps-migration.md`), this ABI goes away. Until then, every detail of how `Card.kind = "plugin:<id>:<view>"` rendering works is explicitly EXPERIMENTAL.

## Per-surface classification table

| Surface | Tier | Today | Required |
|---|---|---|---|
| DB schema | A | `sqlx::migrate!` forward-only at `crates/calm-server/src/db/sqlite.rs:68` | Add refusal of unknown future migrations |
| Sync event envelope | A | `BroadcastEnvelope { id, actor, event }` at `crates/calm-server/src/event.rs:287` — no version | Add `eventVersion` |
| Card payload (kernel kinds) | A | `Card.payload` is `serde_json::Value` at `crates/calm-server/src/model.rs:91` — no version | Per-kind `schemaVersion` + migrator |
| Plugin manifest | A | `manifest_version` hard-gated; `min_kernel_version` parsed but unused | Wire `min_kernel_version` comparison |
| MCP protocolVersion | B | `KERNEL_PROTOCOL_VERSION = "2025-11-25"` sent but not compared | Compare on handshake response |
| MCP capability versions | B | Presence-only check at `mcp.rs:450` | Compare `version` field |
| Terminal daemon framing | B | Length-prefixed bincode at `crates/calm-session/src/lib.rs:87` — no magic, no version | Add `magic + version` to frame |
| REST API | B | OpenAPI `info.version = CARGO_PKG_VERSION` | Independent `apiVersion` constant |
| WS envelope | B | No version | Add `eventVersion` |
| Frontend cache | B | `PERSIST_BUSTER` at `web/src/api/persistConfig.ts:54` | Add `minWebBuildId` enforcement |
| Repo trait | C | Recently split | Stays internal; no version |
| Route handlers | C | — | Stays internal |
| React components | C | — | Stays internal |
| Sync engine internals | C | — | Stays internal |
| TUI adapters | D | — | Mark experimental |
| Claude/Codex parsers | D | — | Mark experimental |

## Event evolution rules (Tier A detail)

The events table is append-only and replay-driven. Every event ever written must remain readable by every future binary. The rules:

- **Add new event types freely.** A new `kind` is always backward-compatible — old binaries that don't recognize it can skip or replay-through it without harm.
- **Add new optional fields freely.** A new field on an existing event's payload is fine, provided the field is optional and existing rows without it still deserialize correctly.
- **Never rename or remove an event type.** The `kind` string is part of the persisted contract. A row with `kind = "card.added"` written in 2026 must still deserialize as `CardAdded` in 2030.
- **Never change the semantics of an existing field.** If `card.payload.color` meant CSS hex on Monday, it cannot mean RGB tuple on Tuesday.
- **Breaking change → new event type + deprecate the old one.** Introduce `card.added.v2`; leave `card.added` in place; teach the projector to handle both on replay. Eventually, when no live writes go to the old type and the audit window has rolled over, the old reader code can be removed — but not the rows.

## Migration policy (Tier A detail)

Schema migrations live under `crates/calm-server/migrations/` and are picked up by `sqlx::migrate!`. Three rules:

1. **Forward-only.** No `down.sql`. Downgrade is not supported; a user who needs to roll back restores from a pre-upgrade backup.
2. **New binary reads old DB.** Handled automatically: every embedded migration applies in order on first boot of the new binary against the old store.
3. **Old binary refuses to read new DB.** This needs explicit code. On startup, the binary inspects `_sqlx_migrations`; if any *applied* migration is not in the binary's own embedded list, it refuses to boot:

   > `database has migration X applied that this binary doesn't know about — refusing to boot; downgrade is not supported`

   Without this guard, an old binary against a new DB would silently proceed, running stale code against rows whose shape it doesn't understand — the exact failure mode this tier is designed to prevent.

## Plugin compatibility rules

Plugin compatibility straddles Tier A / Tier B: the manifest is persisted (Tier A), the running negotiation is cross-process (Tier B). The rules:

- **`manifest_version = 1` is a hard gate.** Already enforced — a manifest with a different `manifest_version` is rejected at install. When we bump to version 2, version-1 manifests will either be auto-migrated at install or refused with a clear message.
- **`min_kernel_version` is honored via semver compare.** The plugin declares the minimum kernel version it requires; on load, the kernel compares its own version against `min_kernel_version` using semver rules. Refusal message: `requires kernel >= X.Y.Z, got A.B.C`. Parsed today but not yet wired to the comparison — wiring it is the immediate next step.
- **Capability versions inside `experimental.dev.neige/*` are matched exactly.** Both sides must agree on the exact version string for a capability to be considered available. Mismatched version = capability absent, warn-logged. This is deliberately stricter than today's presence-only check — a capability at v2 on one side and v3 on the other is **not** compatible.

## PR review checklist

Before opening a PR that touches any surface above, walk through this checklist — it's also the framing reviewers use when reading the diff.

- [ ] **Does this change a Tier A surface?** Then a migration path is present in the PR. For a DB schema change, a new migration file. For a card-payload schema change, a `schemaVersion` bump plus a local migrator helper. For a plugin manifest change, an auto-migration at install time or a deliberate `manifest_version` bump with the gate updated. The PR that breaks the shape is the PR that ships the migration.

- [ ] **Does this change a Tier B surface?** Then the version is bumped, the handshake is updated, and the failure behavior is documented. For an MCP capability, the `version` field bumped and both sides' compare logic updated. For the WS envelope, `eventVersion` bumped and the client's mismatch behavior covered. For the REST API, `apiVersion` bumped independently of the binary version. The reviewer should be able to point at the line of code that handles the version mismatch.

- [ ] **Does this change a Tier C surface?** Then no version is needed — and the PR should explicitly *not* add one. The reviewer's job is to verify there is no leakage: a change that started as Tier C but altered a Tier A or Tier B shape downstream needs to be re-classified.

- [ ] **Does this change a Tier D surface?** Then the `// EXPERIMENTAL: may break without notice` marker is preserved (or added). Nothing from a Tier D surface should appear in `/api/version`, the manifest, or any other negotiation path. The whole point of Tier D is that it stays disclaimed.

## Non-goals (repeated)

- **Not** implementing terminal protocol v2 — that work is tracked in issue #44.
- **Not** enforcing production-grade backward compat. The goal of this policy is *intentional* breakage, not *zero* breakage. A PR that bumps `apiVersion` from 3 to 4 and breaks every existing browser tab is fine, provided the new version is communicated through the handshake and the failure behavior is documented. What's not fine is changing an event payload's shape without bumping `eventVersion` and discovering at runtime that an open tab silently drops half its UI.

The four tiers exist so that "intentional" is a default, not a virtue we have to remember.
