# Recover ownership evidence for squash pushes (#1478)

A squash push that loses its message trailers should pass when every original
PR commit has valid ownership evidence. PR and local audits keep their existing
commit-only behavior, and PR bodies must still preserve trailers for auditability.

Only push commits missing required trailers consult GitHub. Resolve the associated
PR through the commit-to-PR API, require a unique merged PR into this repository's
main with exactly the audited merge SHA, and load its complete original commits.
Validate every original commit before transferring exact-path trailers for paths
that those commits actually changed. Never trust a subject's `(#N)` or a mutable
PR body. Missing evidence remains red; API errors, incomplete pagination, ambiguous
associations, and merge commits fail closed. Existing complete squash trailers do
not require network access.

Both ownership entry points use the same resolver. Supply read-only GitHub tokens
only to their workflow steps; API requests stay on api.github.com, reject redirects,
and have bounded timeouts. The change adds no persisted approval store.

Acceptance uses real temporary Git branches and squash merges for approved and
unapproved source changes, including a later unapproved edit to an already approved
path, unrelated evidence, partial trailers, and multiple pushed commits. API
fixtures pin association identity, pagination, renames, truncation, and failures.
Mutation checks must prove that source validation and evidence scoping cannot be
removed silently. Two independent reviews assess the complete final diff.

Live GitHub merging is outside this local issue implementation: creating a test
squash on production main would mutate released history. Local squash fixtures
and read-only replay of the reported historical commits provide the acceptance
evidence without that mutation.
