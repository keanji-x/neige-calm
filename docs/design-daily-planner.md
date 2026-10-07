# Daily Planner Tracks (#2008)

Replace the desktop homepage's singleton summary assistant with an ordinary Track for each Asia/Shanghai calendar day. Daily Tracks live in the existing hidden System Area and never appear in the sidebar. The homepage reuses the Track report reader, evidence links, file viewer and Planner conversation. The frontend now has one Today composition; `/today/legacy` redirects to the homepage. The earlier singleton API and its persisted report remain unchanged.

## Ownership and lifecycle

The daily feature owns dates and its thirty-second reconciliation loop. Boot reconciles before serving. It creates today's Track once and closes prior daily Tracks; restart repairs the current day without inventing empty missed days. Creation materializes the workspace and cards but sends no model message. The first user message starts the Planner normally.

The existing System Area ensure and Track structure factory remain authoritative. A new migration records kernel-issued `(owner, identity)` creation keys, report read scope, time zone, tool profile and lifecycle owner. Replay validates the immutable metadata before returning the stored Track. Released migrations remain unchanged.

Generic authorization consumes those declarations. It never recognizes an application or template name. The report-planning profile permits report reads and maintenance of its own report; it refuses worker, terminal, lifecycle and plugin mutation tools at both registered and dynamic-plugin entry points. The report persistence boundary also refuses task block changes, preventing report writes from scheduling workers. Agents cannot write a closed daily report; a human can explicitly correct it without reopening the Track.

The template declares `user_creatable = false`. Template admission and the picker use that metadata, so selecting a template cannot imply a workspace read grant. The immutable creation identity is exposed through `neige_track_status`; renaming the display title does not change the day.

## Reports and history

Granted Planners enumerate and read the current reports of user-visible Areas, including closed Tracks. Ordinary Planners, Assistants and Workers receive no workspace grant. Foreign reads do not populate the report write ledger; report writes retain the caller's existing Track binding. Own-area report history remains available through the ordinary Area report view.

The shared history projection reads existing `track.report_edited` events in a half-open Asia/Shanghai day window. It groups edits per visible Track in event-id order and returns first-before / last-after patches plus paged individual edits. Snapshot cursors keep later edits out of continuation pages. Net reverts retain their edit count and full intermediate evidence. A truncated patch is declared, with full edits available separately. Deleted or hidden resources are not exposed, and failures are not presented as an empty day. Code/Git diffs are outside this feature.

The desktop homepage renders the ordinary Track report layout with a single date title and its existing Planner control. There is no additional daily toolbar, time-zone label or duplicate Planner launcher. Report changes follow the document as an appendix, sharing the exact disclosure renderer and typography with the report’s Reference section. Each source report, body diff and individual edit is folded separately; summary changes use labeled fields. Historical dates remain addressable by the `day` query. Mobile work is deferred by the owner; existing phone navigation is preserved.

## Acceptance

1. Concurrent and repeated reconciliation creates one date identity, retains one hidden System Area and emits no duplicate creation/close events.
2. Date rollover closes old daily Tracks and creates a new open Track; automatic creation starts no model turn.
3. Workspace report tools enforce live card/role/Track/Area binding and the persisted read grant. The report-only profile rejects effects outside its stated capability.
4. History includes exact date edges, closed visible Tracks, net reverts and every continuation page. Malformed reads remain errors.
5. Desktop homepage opens the daily Planner, cited reports and history while the sidebar contains no daily entries. Legacy reports remain readable.
6. Targeted Rust tests, full frontend tests, browser checks, generators, authority mutations, text ratchets, relevant Rust gates and fresh L2 reviews converge before delivery.

## Interface ownership decision

The orchestration owner approves the narrow read-only API extension for this authorized feature. Public response types belong to `calm-types` and are exported by the real generator into `fe/core/api/generated/wire.ts`; the OpenAPI emitter owns `fe/core/api/generated/openapi.json`. Commit and PR ownership trailers identify both frozen outputs. Global style contracts and architecture gates remain unchanged.

Review tier: **L2**, because this change crosses persistence and authority boundaries and adds a database migration. Fixed tool identifiers and the User/System Area visibility set are owning-layer protocol contracts; display names confer no privilege.

The card public-entry owner approves exposing `plannerCardIn` so homepage and Track assembly share the registered classifier. The report URL builder stays in its owning domain; the parser keeps the frozen protocol while retiring two legacy identifier occurrences. The vocabulary baseline moves down only.
