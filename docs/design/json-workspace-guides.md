# Contextual workspace guides

Outcome: a new developer can discover the native project, workflow, request,
conversation and result controls without reading external instructions. Guidance
is a single optional callout attached to the real control or adjacent control
group, never a replacement action or a second application layout.

Review tier: L2. This extends client display-preference persistence and adds a
cross-surface frontend composition; two isolated review channels check the final
change. No backend contracts, authorization, migrations or credentials change.

The application owns a versioned JSON catalog of tours and steps. Each step
names a declared anchor, plain-text title/description and an interaction rule.
Adjacent options are one anchor/group, avoiding overlapping explanations. The
UI owns only target registration, availability, progress navigation and the
existing Floating UI callout. Features expose their native anchors and pause
while their own picker is open; no DOM selectors infer template identities or
invoke product actions. Unknown/duplicate definitions are rejected by a schema.

Only a successfully loaded empty workspace enrolls automatically. Existing
projects remain ordinary unless their user requests guidance from the account
menu or explicit startup link. Enrolled browsers see unseen steps as their
controls become available, across Area creation, Track creation, reports and
conversations. Missing or unavailable targets wait rather than count as seen.
Skip ends the whole guide and keeps drafts; replay is explicit.

Progress is an additive display preference, scoped to the existing stable
server database/browser namespace, following existing UI preference ownership.
It never writes project state. Without a confirmed database identity persistence
is unavailable; explicit help still works in memory. Loading/error/empty are
separate states so a failed read cannot enroll an existing project. A revision
and step id identify seen explanations. An ordinary additional explanation edits
one JSON entry and the owning control's anchor if it is new; no gate inventory
or CI roster is added.

Acceptance: schema validity and duplicate rejection; first-empty automatic
entry, existing-project silence, modal/picker yielding, dynamic missing targets,
previous/next/skip, remembered steps and replay; native form/selection/draft
preservation; desktop and compact pointer geometry and reachable actions. A
fresh-context agent independently experiences the real Docker application and
reports whether first development is understandable. Focused browser tests,
frontend lint/build, text ratchets and Tier 1 integration cover delivery.

UI/architecture owner decision: permit createContext in exactly
`web/src/ui/floating-control/guided-tour.tsx`. The generic provider distributes
presentation state into targets inside their own modal ancestry; it receives
all copy and progress through injected contracts. No app domain enters UI.
The existing architecture fixture now iterates the authoritative exception
list and checks every owner plus its rejected sibling, eliminating its parallel
hand-maintained owner roster. An ordinary future context adds one reasoned
exception; no second test inventory. These checks run in the existing test job.

Review refinements: application facts (`has-projects`, `has-work`) are injected
as opaque flags; JSON tour requirements prevent background system work from
claiming first-use guidance. Completed ordinary tours continue to lower-priority
unseen controls; JSON-declared exclusive foreground scenes pause background help
until their form/conversation/settings closes. Confirming an initially unknown
database adopts this visit's explicit Skip/replay and seen choices; switching a
known database resets the visit state.

Fresh-context experience checks found compact-only controls needed their own
wording: the floating new-work button and collapsed `…` options now declare
separate JSON anchors. Report/result explanations are distinct, and Area
creation/editing share the neutral project-settings label. The compact options
owner closes its native popover before opening directory browsing, preventing
an inert but still painted top-layer surface from covering the modal list.
Settings heading spacing and the guided Add glyph retain their owning styles.

## Adding an explanation

Edit `fe/web/src/app/shell/workspace-guides.json`. A tour declares a title,
priority, opaque required application facts and whether its foreground scene
excludes background guidance. A step declares a unique stable id, native target
id, plain-text title/description and `advance: next` or `advance: activate`.
`activate` observes the target's original click; it never clicks on the user's
behalf. Existing groups can receive new copy or reordered steps using JSON alone.
A new control exposes a `GuideAnchor` in its owning feature; combine adjacent
controls into one native group, preserve its original ref/handler, and place the
hint inside its modal ancestry. Pause targets during nested pickers. Conditional
compact controls declare their actual variant target rather than teach a hidden
button. Reload the page after editing the visit's catalog. Bump the version only
when an existing explanation deliberately needs to be seen again.

The visit keeps explicit enrollment/replay/Skip and pending seen progress together.
A single app-owned persistence effect synchronizes reset, seen ids and the final
decision once a stable namespace is available. Automatic enrollment additionally
requires a successful workspace read; explicit display choices can be saved even
while those reads are unavailable. The UI session identity follows replay/actual
database changes, so initial identity confirmation preserves the current step.

Commit-boundary refinement: a render snapshot is not proof that its display
namespace is still current after the parent scope provider's layout effects.
`UiPreferences.commitGuideProgress` accepts an opaque current namespace token,
rejects stale/unconfirmed scopes, stages all progress fields before one layout
notification, and returns acknowledgement only for the accepted live scope. The
app clears pending reset only after that acknowledgement. The token also follows
the existing origin/user namespace, not just database identity. This is one
persistence entry point rather than independent effect guards and field writes.

Visit identity is acknowledged, not inferred: its `acceptedScope` starts empty
and is set only by a successful scoped commit. A cached render identity that the
real scope provider invalidates never counts as an accepted visit. Thus first
confirmation (even of a different database than the cache) adopts pending reader
choices; only a change after a confirmed visit creates a new visit epoch.
