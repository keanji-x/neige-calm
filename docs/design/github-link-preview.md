# Desktop GitHub link previews

Outcome: desktop readers can inspect GitHub Issue and PR links in chat replies and
report prose without leaving Neige. Mobile retains its current rendering and navigation.

A single GitHub link system owns lazy reads and hover presentation. The app injects
its existing authenticated, recovery-aware API transport. The core model admits only
HTTPS github.com Issue/PR URLs and constructs structured preview requests. Reports use their existing external-link preview and substitute GitHub summaries
for iframe content; other external links retain their policy.

The protected read-only API uses GitHub CLI authentication on the server, never in
the browser. It constructs fixed REST paths from validated owner/repository/number
fields, invokes no shell, uses an explicit process environment, and bounds command
runtime, output and concurrency. No arbitrary URL proxy, persistence or token API.
Private repository visibility follows the signed-in server operator's GitHub access;
Neige's existing protected surface is owner-only. Failure never exposes CLI stderr.

Acceptance: Issue/PR state, author, labels, excerpt and PR change counts; no requests
before a desktop hover/focus; normal chat navigation; an explicit GitHub action in
the report card; Escape dismissal; no card or reads on compact/touch clients; loading,
retry and safe error display. Invalid hosts, paths and identifiers cannot reach gh.

Review tier L2: a new authenticated read uses the server's GitHub identity. Two
independent review channels must check credential handling, boundaries and callers.
The orchestrator approves the new systems/github-links ownership entry, its narrow
context-owner exception, and regenerated OpenAPI output. Existing frozen runtime
interfaces and global styles stay unchanged.
