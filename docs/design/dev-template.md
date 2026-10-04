# Development template

Outcome: replace the issue-required `issue-development` template with `dev`. Development requirements come from the user; an optional issue adds requirements and discussion. The template owns task orchestration, repository identity checks, PR publication, CI waits, and merge authorization. Target repository AGENTS.md and CONTRIBUTING.md own development policy, review tiers, verification, and previews.

Review tier: L2 because the template identity is persisted and merge authorization must remain unchanged.

Inputs: optional issue URL with its existing derived repo/issue number fields; merge approval continues to default to hold-for-ratify. No issue means no GitHub issue reads, comments, or closure. Blank optional formatted fields emit no values; malformed nonblank values still block submission. When issue repo is present, retain the origin cross-check before writes. Without issue, use the track checkout and user request; surface actual repository ambiguity before writes.

Rename all live registrations and callers to dev; preserve released migrations and historical design records. A new migration updates tracks and area defaults while preserving bound input, plugin scope, report body, approval, planner snapshots, and execution history. Existing report methods remain as saved; do not rewrite user reports.

Acceptance: dev is the registered template; no-issue creation and form submission succeed; issue URL still derives repo/number; invalid nonblank URLs remain errors; merge hold stays the default; repository mismatch remains a blocking decision; upgraded issue-development tracks and area defaults resolve dev without losing payloads; repository policy is delegated rather than copied.

Verification: focused production form and server creation tests, migration tests, relevant frontend/browser checks, repository text gates, and two independent read-only review channels. Mutation verification pins preservation and omission invariants where load-bearing.

Upgrade and rollback: existing tracks retain their creation-time working method. The old public template ID is retired after migration; callers must select dev. Downgrading to a binary that only registers issue-development requires restoring the pre-upgrade database backup; prefer a forward fix.
