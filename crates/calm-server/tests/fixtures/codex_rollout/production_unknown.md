# Production unknown rollout records

Captured read-only from the shared supervisor's production rollout:
`/home/kenji/.local/share/neige-next/data/codex-home/sessions/2026/10/07/rollout-2026-10-07T09-29-46-01a113fb-1b05-7b01-aab7-7e61b15833a7.jsonl`.
The session_meta reports cli_version `0.159.2` and originator
`neige-calm-shared-supervisor`; the installed production `codex --version`
also reports `codex-cli 0.159.2`. No Codex process or E2E was launched to
obtain these records, and no external/codex source supplied the protocol.

The first world_state and token_usage_record were selected. Every string
value inside payload was replaced with `[redacted]` using jq walk; keys,
containers, booleans, numbers, timestamp, ordinal and top-level type are
unchanged. This removes instructions, local paths and session/response IDs
while preserving the production payload shapes.
