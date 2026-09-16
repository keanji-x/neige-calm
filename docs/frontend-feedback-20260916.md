# Frontend feedback, 2026-09-16

The dedicated preview uses the existing authenticated backend and data. The
original working directory is preserved.

## Navigation and activity

Report chapters and conversation exchanges use the same domain-free
`ui/edge-navigation` component. Features provide IDs, accessible labels, preview
text and jump handlers; the primitive owns compact markers, pointer magnification,
preview timing, bounded scrolling, roving focus and touch targets. The first
conversation exchange immediately has a marker; an empty conversation invents none.

`ui/activity-indicator` renders attention in red, working as a spinner, unread in
blue, and quiet without a marker. Features determine the corresponding state.
Read receipts are browser-local, namespaced by the backend instance, monotonic
across writes and preserved across reloads. A hidden tab or a conversation whose
new history has not loaded does not acknowledge the update. Accessible status
uses descriptions without changing the navigation control's name.

## Inventories

Tasks and Cards group by current execution. A card linked to a task uses that
task's current status; a surviving worker process does not keep completed work
in the active group. Standalone cards use their available runtime state. Unknown
states remain explicit rather than being called completed.

Only in-progress groups expand initially. Other groups disclose their counts and
original rows/actions on request. Derivation owns sorting; desktop and mobile
painters preserve the supplied projection, including every row and action.

Module titles, group labels and row names share one left text boundary. Counts
share one right boundary and tabular numerals; disclosure arrows occupy their
own trailing column. Status and worker type use stable columns. Expanded groups
have a rounded surface using the Report background token and a hairline between
the heading and its rows. Collapsed groups share the sidebar's 28px rhythm;
expanded content starts 4px below its heading and ends with 8px of space.
Lists scroll inside their group; simultaneously expanded groups share a
bounded module height so the following modules remain reachable.

`ui/list-typography` owns the type roles for both surfaces, including selection
emphasis. Area names, status groups and conversation names use the group role;
Track names and expanded task/card names use the primary role. Hosts own layout
and state, without redefining font size or weight locally. The redundant Track
header's independent-task button is removed; its existing action-menu entry remains.

## First-message model selection

The draft exposes model and reasoning controls before sending. Its selection is
locked once creation starts or an attempt remains unconfirmed, including late
selection events from a previously opened menu. The selection is submitted in
the same idempotent request as the first message.

Conversation creation stores the selection in the card payload inside its mint
transaction, before dispatch. Model fields join the operation's request identity;
a retry cannot silently change the selection. Omitted values serialize exactly
as the legacy seed did, preserving saved operation hashes. Only the human actor
may choose a model. No database migration is introduced.

The backend advertises `conversationCreateModel`. The preview leaves first-message
selection disabled until this capability is present, because older servers would
ignore the added fields. Existing clients and default-only requests remain valid.

## Verification and ownership

Acceptance covers real route requests and retries, unread completion/readback,
complete inventory projections, actual browser geometry and hit targets, shared
outline behavior on fine/coarse pointers, generated API output, and the repository's
relevant frontend and Rust gates. Load-bearing model delivery and capability
assertions are mutation-checked in an exclusive worktree.

The user's explicit request authorizes the narrow `ui/edge-navigation`,
`ui/activity-indicator` and `ui/list-typography` inventory additions and generated
first-message model API contract; approved by the orchestrator in #1713.
Dependency directions, global style contracts and gate rules are unchanged.
Required ownership trailers are preserved in the commit and PR squash body.
