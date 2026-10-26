# Dialog

## Visual contract

`Dialog` owns overlay, panel, header and body chrome so every modal uses one stacking and dismissal policy; `wide` and a pushed child view select the wide variant. `ConfirmDialog` composes the same panel and exposes only a danger/primary action variant so confirmation call sites cannot fork the modal mechanics.

## Accessibility contract

The panel alone owns `role="dialog"`, `aria-modal` and an accessible name from its title so assistive technology sees one modal surface. Initial focus resolves explicit ref → first visible focusable → panel; Tab is trapped, background siblings are inert, and close restores the latest override or prior focus. Child views form a LIFO stack; `pushView` returns an idempotent ownership disposer so one field cannot pop another field's view. Escape pops the top child before closing. The primitive avoids native `<dialog>` because browser top-layer/backdrop behavior would bypass the app-owned portal and inert policy; it avoids cached focusables because controls can appear after async rendering, and disables overlay dismissal for a child view because a nested workflow must cancel explicitly.

## Test contract

Consumers locate it by dialog role and accessible name. DOM contract tests assert the rendered ARIA surface, retained child content, dynamic focus trapping, exact inert restoration, detached-trigger handling, nested Escape ownership, stack disposal, and the load-bearing inert-before-restore cleanup order so implementation-preserving refactors remain free.

## Motion and source contract

`origin` is required on Dialog and ConfirmDialog: `DialogOrigin` excludes paired edges from Motion's `LiftOrigin`. Callers declare a stable unpressed anchor ref with an explicit center fallback, or an explicit surface center for navigation/system intents without a public source. Confirm forwards it unchanged. The declaration's kind/ref locks for one open presentation; initial focus is independent. Motion alone tracks coordinates and freezes detached, hidden or replaced sources; a first unavailable source stays centered for that cycle.

An unanimated CSS Module frame owns final width and height, while its single paint panel uses `useSurfacePresence` lift. Layout/viewport changes rebase through the existing Motion owner; child push/pop keeps the same mounted panel and does not acquire a second entrance. `present=open`: closing cancels effects and unmounts immediately, with no business or resource wait.

Escape uses public Layer dismissal/depth, including IME and nested native layers. The legacy escape marker remains a lazy interoperability fence. Body inert cleanup still precedes focus restoration. Global registered chrome uses existing style tokens; the new local frame requires no manifest entry or caller registry. Ordinary callers edit only their intent/callback and tests, and incur one local stable box/ref, without idle timers.

`purpose` is required: `info` permits backdrop dismissal; `form` protects drafts
from backdrop clicks. Escape and Close remain explicit dismissal in both modes.
Pushed child views always block backdrop dismissal. `ConfirmDialog` declares
`info`: dismissing a confirmation cancels safely. Every new caller selects its
purpose; TypeScript enforces this without a separate caller registry.
