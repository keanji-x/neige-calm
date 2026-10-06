# Manual conversation compaction

Outcome: `/compact` compresses the current Codex conversation's context without sending a chat message or creating a conversation. Slash commands use distinct icons: new (plus), side (chat), compact (inward arrows).

Review tier: L2, because compaction changes provider-owned durable context. The REST route must admit only human users and harness-backed cards. Provider policy stays in PlannerBackend; the generic composer receives an optional callback.

The harness serializes compaction with its observation commands and issuance lock. It refuses running turns, pending messages, pending rewinds, sealed threads and missing history. Codex's `thread/compact/start` acknowledges submission; a persisted `compacting` phase blocks issuance through both submission and completion. The live state carries a required turn id once it starts. Maintenance completion is not appended as a reply to the previous user message. A watchdog fences unconfirmed starts and hung compactions. Unknown outcomes wedge the harness instead of resuming issuance. No automatic retry. Unsupported providers return a clear refusal.

Acceptance: protocol request is correct; compaction is not sent as text; active/queued/rewound/shutting-down conversations refuse it; provider refusal is surfaced; unrelated notifications cannot complete it; the menu uses distinct icons and supports keyboard selection. Run focused Rust/frontend tests, generated API contracts and required gates. Review authority, persistence, architecture boundaries, duplicated policy and generic application assumptions through two independent review channels.

Compatibility: REST API 23 and frontend compatibility floor 44 introduce the `compacting` phase. Regenerate wire and OpenAPI outputs. Older snapshots retain their existing decode path; an interrupted compaction snapshot recovers wedged and requires reset, without resubmitting or guessing whether compaction completed. The new live maintenance states deliberately share one persisted phase; recovery never needs to invent a missing turn id. Roll back after compaction settles, since older binaries cannot read the new phase.

Owner change request: core/api adds `compacting` to its runtime phase schema and regenerates `generated/wire.ts` and `generated/openapi.json`. The requested capability requires this narrow contract extension. Orchestrator decision: accept these three paths, bump frontend compatibility to 44, and keep all command and provider policy outside the generic API layer.

Review repair: maintenance Item and PlanUpdated events are excluded from conversational transcript by the owning harness lifecycle, so compaction does not create a foreign-turn suffix that prevents editing the latest user message. The REST regression runs existing real send/echo/completion paths, compaction with an actual maintenance item, then replacement. It reproduced 409 before the fix. No provider item-type or application identity is special-cased.

Issue: #2301. Rebased assembly uses app/conversations/store and the committed callback contract introduced on main; Planner route admission consumes planner_cards and typed extractors.
