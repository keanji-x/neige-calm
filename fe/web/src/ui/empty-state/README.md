# Empty-state visual standard

Every page/panel empty state has exactly three required content fields:

```ts
type EmptyStateSchema = Readonly<{
  icon: ReactElement;
  title: string;
  description: string;
}>;
```

The layout is always Icon + Title on one row, then Description below. There is no button, action
callback, children or arbitrary content slot. `variant` and `align` control
presentation only. Features own the three content fields; the UI primitive
owns their layout, typography and decorative icon treatment.

| Surface | Icon | Title | Description | Spacing |
| --- | --- | --- | --- | --- |
| Page | Registered 16px icon, beside the title | 20px/24px, medium, primary text | 14px/1.65, secondary text, at most two lines at ordinary widths | 8px icon-to-title; 8px heading-to-description |
| Panel | Registered 16px icon, beside the title | 16px, medium, primary text | Same body role, shorter copy | 8px icon-to-title; 8px heading-to-description |
| Compact | Registered 16px icon, beside the title | 14px/20px, medium | 12px/1.5 | 6px icon-to-title; 4px heading-to-description; 8px block padding |

Existing page-header creation actions and conversation composers remain the
places to act. Empty states must not repeat them. Recovery controls belong to
loading/error/permission states, which are separate from this schema.

Page copy is bounded at 22rem; panel copy at 18rem. Keep the heading as one
short clause and the explanation as one sentence. Do not truncate copy on
narrow screens or when the reader enlarges text. The visual is decorative and
must not carry information missing from the text.

Placement follows the surface: an empty report is centered in its reading
column; Today stays aligned with its date and overview; recipe and settings
lists stay aligned with their headings. Conversation placeholders are centered
inside the available transcript area, above the existing composer. Avoid a
second container around the complete message, oversized illustrations and technical setup instructions.

A first-use state explains what will appear and how to start. A legitimate
zero-result state states the outcome without inventing work. Loading, running,
connection failure and missing permissions remain distinct. An active
conversation uses the activity indicator in the visual slot; it must never
show the invitation to start a new conversation while already working.

This is static content, not a live region. Loading and failure announcements
remain with the caller. It adds no focus handling, persistence or animation.
`MobileListEmpty` supplies the required list item and forwards the three fields
to `EmptyState` with `variant="compact"`. Desktop callers use `EmptyState`
directly. The list adapter owns no typography or empty copy.
Projection markers annotate the title only, preserving exact view-model text.

All confirmed normal absences use this component: reports, conversations,
inventory, navigation, calendar days, files/folders/diffs, directory and composer
search, charts, records/tables, execution history, report previews, historical
days and report changes. Loading, failures, invalid references,
missing permissions and pending synchronization keep their owning state UI.

## Pinned dependency integration

Astryx 0.6.3 declares ChatComposerTrigger.emptySearchResultsText as a string,
but its useTriggerMenu renders that value directly as a React child (lines
642–653 of the installed source). `astryx-trigger.tsx` is the single explicit
adapter for that declaration mismatch. It passes the shared component directly;
it does not insert a fake option, replace keyboard handling or mutate the DOM.
The expected TypeScript error makes a future widened declaration require this
adapter to be revisited. Real browser checks cover both @ and / empty menus,
including zero options and unchanged input. No dependency files are patched.

Owner registration decision: the requested consistency work needs one neutral
UI primitive shared by independent features. Accept the narrow directory
registration in module-file-inventory.yaml; preserve all existing entries.

## Consolidation acceptance and review

This consolidation is L2 because it spans many independent surfaces. It changes
presentation only; authority, persistence and data-read ownership stay intact.
Two independent review channels check the final isolated tree.

Acceptance: every confirmed normal absence uses the same required icon/title/
description schema, including compact lists and no-match search. No action or
focusable element may appear inside it. Loading/error/permission/integrity states
remain distinguishable, and a pending or failed read must not be claimed empty.
The title alone retains inventory projection markers. Desktop, 390px and 320px
checks cover wrapping and reachability. The full screenshot catalog exercises
production page/component entry points rather than copies of their rendering.

## Test responsibilities

The browser scenario file owns viewport changes and user flows. Its fixture
module owns explicit test data and component staging; the assertion module owns
schema/interaction checks; the capture module owns rendering readiness and file
export. Production rendering and the pinned Astryx adapter remain authoritative.

The approved empty-state typography and pixel gaps are preserved on current
main's token scale by composing its existing font/spacing tokens locally. No
new component props or global tokens are added for this migration.
