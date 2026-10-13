# Manual conversation compaction

Outcome: `/compact` compresses the current Codex conversation's context without sending a chat message or creating a conversation. Slash commands use distinct icons: new (plus), side (chat), compact (inward arrows).

Review tier: L2, because compaction changes provider-owned durable context. The REST route must admit only human users and harness-backed cards. Provider policy stays in PlannerBackend; the generic composer receives an optional callback.

The harness serializes compaction with its observation commands and issuance lock. It refuses running turns, pending messages, pending rewinds, sealed threads and missing history. Codex's `thread/compact/start` acknowledges submission; a persisted `compacting` phase blocks issuance through both submission and completion. The live state carries a required turn id once it starts. Maintenance completion is not appended as a reply to the previous user message. A watchdog fences unconfirmed starts and hung compactions. Unknown outcomes wedge the harness instead of resuming issuance. No automatic retry. Unsupported providers return a clear refusal.

Acceptance: protocol request is correct; compaction is not sent as text; active/queued/rewound/shutting-down conversations refuse it; provider refusal is surfaced; unrelated notifications cannot complete it; the menu uses distinct icons and supports keyboard selection. Run focused Rust/frontend tests, generated API contracts and required gates. Review authority, persistence, architecture boundaries, duplicated policy and generic application assumptions through two independent review channels.

Compatibility: REST API 23 and frontend compatibility floor 44 introduce the `compacting` phase. Regenerate wire and OpenAPI outputs. Older snapshots retain their existing decode path; an interrupted compaction snapshot recovers wedged and requires reset, without resubmitting or guessing whether compaction completed. The new live maintenance states deliberately share one persisted phase; recovery never needs to invent a missing turn id. Roll back after compaction settles, since older binaries cannot read the new phase.

Owner change request: core/api adds `compacting` to its runtime phase schema and regenerates `generated/wire.ts` and `generated/openapi.json`. The requested capability requires this narrow contract extension. Orchestrator decision: accept these three paths, bump frontend compatibility to 44, and keep all command and provider policy outside the generic API layer.

Review repair: maintenance Item and PlanUpdated events are excluded from conversational transcript by the owning harness lifecycle, so compaction does not create a foreign-turn suffix that prevents editing the latest user message. The REST regression runs existing real send/echo/completion paths, compaction with an actual maintenance item, then replacement. It reproduced 409 before the fix. No provider item-type or application identity is special-cased.

Issue: #2301. Rebased assembly uses app/conversations/store and the committed callback contract introduced on main; Planner route admission consumes planner_cards and typed extractors.

## Claude Planner

`PlannerBackend::compact_start` spawns one `claude -p --resume <thread>` through the Claude session's turn path (seal checks, `--version`, marker `stop`, instructions file, settlement) on the card's model and under `never`. Its single user line is `/compact`: the Agent SDK documents slash commands as ordinary inputs and `system/compact_boundary` as the record a compaction emits. The CLI reference says `--disable-slash-commands` disables all skills and commands, and the recorded 2.1.280 init lists no command under it, so only the compaction spawn omits the flag. Its init check requires `compact` in `slash_commands` in place of the turn's no-skills check; the Skill tool stays off through `--tools`.

The spawn emits only `TurnStarted` and `TurnCompleted` for its minted turn id, so the harness's `Compacting` → `CompactionRunning` → `TurnCompleted` handling is unchanged. It forwards no item, delta or usage reading and stores no outcome row, so it is neither a transcript turn nor a rewind anchor, and `last_turn_id` stays the person's turn. A success result counts only after a `compact_boundary`; otherwise the compaction fails and the harness wedges, as for Codex. Every failure before the `/compact` line is written (no session yet, unpinned or unanswered CLI, unresolvable model, a setup `stop` or credential mint that failed, a turn still running, a spawn that did not start) submitted nothing and restores the previous phase; a refusal among them is a `BadRequest`. Only once the line may have reached the CLI does a failure wedge the harness.

A later turn resumes the compacted session as usual. A rewind still cuts at the last record of the person's turn before the removed one. When a compaction lies between them, that anchor precedes the boundary, so an accepted cut restores the uncompacted history up to the anchor, which still matches the transcript. The existing dry run (`--resume-session-at` / `--resume-drops-turn`) lets the CLI judge that cut first; a refusal is a 409 and changes nothing.

The web composer still offers `/compact` only when `scopeProvider === 'codex'` (`fe/web/src/app/router/public.tsx`); exposing it for Claude is a separate frontend change.
