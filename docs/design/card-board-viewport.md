# Board-sized card surfaces (#2760)

The board owns two different rectangles: a grid cell (registry defaults or the
user's resize) and the board's current scrollport. A renderer must not infer the
second rectangle from application selectors or the global window.

Cards may opt into `CardEntry.viewport = 'board'`. The board measures its own
scrollport with ResizeObserver and supplies its block size as an inherited CSS
property. The opted-in card's surface has a maximum block size of that scrollport
and sticks to its top while the original grid cell passes through it. The grid
cell, stored positions, resize handle and active-card reveal retain their existing
ownership and dimensions. Other entries retain their full-cell surfaces.

The file card opts in. Its header and search row share the bounded surface with
an independently scrolling CodeMirror editor. CodeMirror remains authoritative
for matching, selection and revealing text. No focus substitution, search scroll
repair or renderer knowledge of the board is needed. Native manual board scrolling
continues to move grid cells; sticky surfaces stay inside their own cell bounds.
The resize observer is disconnected on board unmount and updates on viewport
changes and after a hidden board is shown again.

Scoped styles change request: extend `fe/web/src/styles/track-grid.css`, owned by
styles, with the opt-in surface rule only. Approved here by the task orchestrator
under the user's request to fix #2760; no global class or existing default changes.
The commit and PR must preserve the exact-path OWNERSHIP-CHANGE trailer.

Acceptance uses the real BoardHost, registered file card and CodeMirror, including
an 8,000-match file, 1400×500 window with a 368px board, and a real pointer resize.
Check controls and selected DOM text against the board/editor clips and pointer
hits, keyboard focus, unchanged source and grid height, manual board scroll,
visibility/reopen and viewport changes. Keep existing full-card and sibling-card
browser coverage. Verify the same behavior in a normal production bundle.
