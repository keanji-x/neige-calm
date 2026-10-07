# Issue creation and discovery (#2364)

Review tier: L2 because creation writes externally and recovery crosses caller
isolation and persistence boundaries. The Planner arranges two review channels.

## Contract

Register `gh_issue_create` (repo/title/body/idem, nonblank) and `gh_issue_search`
(repo/query, state open/closed/all default all, limit 1..100 default 30, optional
attempt). New tools normalize owner/name and optional host/owner/name selectors;
they reject URLs, traversal and endpoint syntax. The gh configured host remains
the credential trust boundary. Creation does not accept labels or assignees.

Creation uses a structured repo/idem identity and binds the full normalized
request in semantic context. A hidden versioned marker binds the current plugin,
trusted track/card, repo, idem and request. Preserve historical comment markers.
All user data travels as argv or structured JSON, never shell source.

Create through the structured REST API and validate its real number and URL.
Recovery enumerates every repository issue page with state=all, excludes PRs,
and requires exactly one complete body/title match. Both verdict and output
probes use the same implementation; a failed second probe cannot emit success.
Query errors, invalid JSON, incomplete fields, page failures and ambiguous
matches are Unknown. Zero matches after a complete enumeration is NotLanded.
No path repeats a write. Typed created events expose issue_number/issue_url;
searched events carry an artifact and inline stdout.

View keeps issue.read but returns JSON number/url/state/title/body/labels. All
default and attempt identities move to a structured v4 key so old body receipts
cannot replay. Search returns a JSON array with the same fields, using bounded
GitHub issue search, state filtering and attempt-based refresh. It is not the
complete recovery oracle and does not make search-plus-create atomic.

## Limits and verification

Permanent keyed dedup and terminal failure behavior remain unchanged. Unknown
does not become automatically retryable. Marker edits/deletion/copying and API
enumeration without a stable snapshot limit remote exactly-once guarantees;
do not blindly create with a new idem after an ambiguous failure.

First reproduce missing registration and the view contract through the actual
MCP entry point. Cover dedup/content conflicts, caller isolation including two
cards on one track, closed second-page recovery, real number/URL and no double
write; exercise every Unknown class including output-probe failure. Shims model
external API responses only. Mutation-check production caller marker, Unknown,
paging and output URL with predicted complete failure sets and safe restoration.
Run focused lower/stdio/affected regressions, fmt and the actual API generator.
The Planner's gates cover ratchet/contract/quick/E2E separately.
