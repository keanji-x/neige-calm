# Track plugin authorization

L2: this change adds persisted, user-controlled authority on a Track. Implements [#3481](https://github.com/keanji-x/neige-calm/issues/3481).

## Outcome

A Planner reads installed/running plugin state and its own Track authorization through a kernel tool. For an enabled, generally available plugin it can request extra Track access using the existing ask/answer drawer. The kernel supplies the question and choices; only an authenticated user option authorizes a change. A refusal is not persisted as a permanent denial. A later request may ask again. Approved access survives sessions; an already approved request returns state without asking again. Revocation uses the same explicit user choice.

The owning plugin remains unchanged. Extra access never propagates to other or child Tracks, never grants a role, never enables a globally disabled plugin, and never exposes owner-only plugins. A missing/unavailable owner still fails closed. Both discovery and invocation read current grants, with no session cache. Plugin authorization is not a schedule creation receipt.

## Boundaries

A new SQLite table holds per-Track/plugin decisions and a revision. The tagged ask captures the canonical installed declaration digest (runtime tool materialization is excluded) and decision revision. The authenticated answer validates both and writes the decision in the same transaction as ask.answered. Stale prompts cannot restore revoked access or approve a replaced manifest. Ordinary questions/free text cannot grant access. Decisions retain their revision across plugin removal; access is bound to the exact manifest digest, so a changed plugin requires a new user approval.

## Acceptance

Exercise real MCP discovery/calls and the authenticated HTTP answer route: denied before approval, allowed afterward, same-Track isolation, role checks, no inheritance, duplicate approval, repeated refusal, typed-text refusal, stale prompts, changed/disabled plugins, durable restart, revocation and unchanged owner tools. Existing ask rendering handles action options; client schemas must retain the new action. Run focused tests, critical fence mutation, both independent L2 reviews, text gates, Rust quick and contracts, frontend lint/build and complete unit tests for the shared event contract. No production restart or real Codex E2E on the shared host.

## Planner contract

`neige_plugin_access` is Planner-only. `{"action":"status"}` distinguishes global enabled/running state from owner, standard, extra, or missing authorization. `{"action":"request","plugin_id":"calendar","enabled":true}` opens a kernel-authored question and returns `awaiting_user` with its ask ID; the Planner ends the turn. Approved repeats return `already_set` without another question. `enabled:false` asks to revoke extra access, including when the plugin is globally disabled. No caller-supplied Track, questions or options are accepted.

The existing authenticated ask-answer route commits the decision before publishing the answer wake. No-op requests use the event writer's typed rollback channel and publish nothing. The core Planner prompt stays unchanged; the new tool's 709-byte description and 181-byte schema add a fixed 890-byte declaration cost to the existing aggregate budget.

Current Codex discovery bootstraps all running plugins, and an access request requires its plugin to be running. Claude and ACP reattach MCP per issued turn. Therefore this permission change needs no global plugin reload or conversation reset; invocation still checks current authority after revocation.
