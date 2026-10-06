# Issue communication through gitforge

Planner issue communication uses the existing `gitforge` plugin and kernel
forge-action runtime. No plugin subprocess receives forge credentials. Plugin
scope, role checks, the recorded operation, and the durable pre-write handshake
remain the authority boundary; this feature adds no permission bypass.

## Contract

- `gh_issue_comment` requires `repo`, a positive `issue`, nonblank `body`, and
  nonblank `idem`. A logical comment keeps its idem and body on every retry;
  another comment uses a new idem. The kernel scopes the operation to its caller.
- The body carries a hidden SHA-256 marker derived from the structured tuple
  `(plugin_id, track_id, card_id, repo, issue, idem, body)`. The kernel supplies
  caller identity as typed context to builtins or private metadata to local stdio
  lowerers, never as user arguments. Missing or invalid scope is rejected. Recovery matches the complete posted body, never
  an unmarked human comment or just a shared text fragment. User strings are
  argv values; the jq comparison uses a JSON-escaped string literal.
- The write is parked before execution, as merge and close are. A successful
  recovery query means landed only for an exact match. No match means not
  landed; query errors or unexpected output mean unknown. The existing runtime
  records unknown as a gate-infra failure, and never posts another comment.
  Editing/deleting the posted body prevents proof of landing.
- Completion is recorded by the existing operation lifecycle and result receipt;
  no new domain event or persisted schema is introduced.
- `gh_issue_comments` returns the discussion as JSON, requires `repo` and positive
  `issue`, and accepts `attempt` for fresh reads. `gh_issue_view` also accepts
  `attempt` while preserving its existing default key and body-only result.

## Acceptance

Exercise the real lowerer and Planner MCP entry point: discovery, recorded parked
write, exact body, repeated request deduplication, conflicting body rejection,
multiple comments, fresh discussion/body reads, and unknown/not-landed/landed
recovery. Check malformed inputs and shell/jq metacharacters. Mutation-check the
body-sensitive idempotency invariant. Review authority and recovery independently.
