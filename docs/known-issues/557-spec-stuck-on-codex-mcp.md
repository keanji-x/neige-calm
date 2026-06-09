# 557: Spec Stuck on Codex MCP

## Symptom

A shared spec runtime can stay stuck in `turn_running`: `runtimes.handle_state_json.phase` never advances past the active turn, while the DB has recent `events.kind = 'codex.hook'` rows for the turn but no `hook.codex.stop` payload kind.

## How to reproduce

1. Start the dev container stack so the calm-server container is running.
2. Run at least one spec card through a full turn that invokes the shared Codex daemon and makes an MCP tool call.
3. Leave the live system in that state and run the diagnosis script from the host.

## Diagnosis script

`scripts/diagnose-557-spec-stuck.sh --container <name>`

Example output:

```text
Container: neige-calm-569-server-1
Database: /var/lib/neige-calm/calm.db
Window seconds: 1800

Codex hook counts:
hook_kind                         count
--------------------------------  -----
hook.codex.permission_request         1
hook.codex.post_tool_use              3
hook.codex.pre_tool_use               4
hook.codex.session_start              1
hook.codex.user_prompt_submit         1

Shared spec runtimes:
id                                    card_id                               status        phase          updated_at_ms
------------------------------------  ------------------------------------  ------------  -------------  -------------
runtime-example                       card-example                         running       turn_running   1781010000000

BUG #557 PRESENT
```

## What this does NOT prove

This script only observes that the symptom has already happened in a live system. It does not prove root cause. The current working boundary for follow-up investigation is between the "MCP tool call hang" layer and the "shared daemon hook chain" layer.

## Related

issue #557
issue #555
issue #570
