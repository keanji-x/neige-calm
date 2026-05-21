# `web/src/ui` — primitive layer

## Purpose

This directory holds the app's UI primitives — small, presentation-layer
components that wrap a single ARIA pattern (dialog, menu, confirmation
prompt) and own its visual contract, accessibility contract, and test
contract. The goal of factoring these out of `shared/components` is to
make the keyboard / focus / role story a single audited thing per
primitive rather than a per-call-site free-for-all. See issue
[#60](https://github.com/keanji-x/neige-calm/issues/60) for the rationale
and the slice plan. The broader a11y contract this layer feeds into
lives in [`docs/a11y-contract.md`](../../../docs/a11y-contract.md);
per-primitive contracts live here so call sites and reviewers can find
them next to the implementation.

## Contract template

Every primitive in this directory MUST document three contracts. New
primitives should follow the template below; reviewers will block PRs
that ship a primitive without filling all three in.

### Visual contract

- Which design tokens (`var(--…)`) the primitive consumes. Light/dark
  theme support comes from the tokens — no hard-coded hex / rem values.
- Whether the primitive reuses an existing CSS class (e.g. `.modal-panel`)
  or introduces a new one. New classes should pair with a token-only
  ruleset in `web/src/calm.css`.
- No ad hoc colors, radii, shadows, or focus rings at call sites. If a
  call site needs visual variants, expose them as a prop on the
  primitive, not as inline styles.

### Accessibility contract

- The expected ARIA `role` and how the primitive computes its accessible
  name (typically from a `title` prop or `aria-label`).
- Keyboard behavior: which keys are intercepted, what they do, and where
  the canonical state machine lives.
- Focus management: initial focus target, restore target, whether a
  focus trap is in effect, and the Escape contract.
- Anything the primitive deliberately does NOT do (e.g. "click-outside
  is disabled in sub-mode") should be called out so call sites don't
  layer their own version on top.

### Test contract

- The selector test code (unit + e2e) uses to find the primitive — always
  `getByRole(role, { name })` per `docs/a11y-contract.md` §8.1. Never
  add a `data-testid` for selector purposes; test IDs in this layer are
  only for harness-internal references that the primitive itself ignores.
- The unit test file(s) for the primitive and what they lock down
  (keyboard behavior, focus, role + name, child-view stack, etc.).
- What's deferred to higher-layer tests: e2e axe scan results, visual
  regression, real-browser keyboard semantics.

## Rules

These are the invariants for the primitive layer. Future ESLint rules
will enforce the mechanical pieces.

- **Tokens only.** No primitive may hard-code colors, radii, shadows, or
  motion durations. Use `var(--…)` exclusively. If the token doesn't
  exist yet, add it to `calm.css` in the same PR.
- **One ARIA pattern per primitive.** A primitive owns exactly one role.
  Compositions (dialog containing a menu) layer at the call site.
- **Contract test mandatory.** Every primitive ships with at least one
  test that asserts its ARIA contract end-to-end. The test exists so a
  refactor to an adjacent primitive can't silently regress this one.
- **App code does not query primitives by raw `role="…"`.** Call sites
  import the primitive and let it own the role; tests select with
  `getByRole(role, { name })`. A future lint rule will flag `role="…"`
  string literals in JSX outside `web/src/ui/`.
- **No re-export shims.** When extracting a primitive from
  `shared/components`, update all callers directly. Stale re-exports
  rot and let two import paths drift apart silently.
- **No business logic.** Primitives know about focus, keys, and roles —
  not domain models. If a primitive needs a callback, take a function
  prop; do not import from `api/` or `cards/`.

## Dialog

The modal dialog primitive. Source:
[`Dialog.tsx`](./Dialog/Dialog.tsx). Tests:
[`Dialog.test.tsx`](./Dialog/Dialog.test.tsx),
[`Dialog.contract.test.tsx`](./Dialog/Dialog.contract.test.tsx).

### Visual

Reuses the existing `.modal-panel` / `.modal-overlay` / `.modal-head` /
`.modal-title` / `.modal-close` / `.modal-body` rulesets in
`web/src/calm.css`. All sizing, color, and shadow come from the
established calm tokens — no class names changed during the
`shared/components/Modal` → `ui/Dialog/Dialog` move. A wide-panel
variant is available via the `wide` prop (or implicitly when a child
view is pushed); both add `.modal-overlay-wide` + `.modal-panel-wide`.

### Accessibility

Implements the modal dialog pattern documented in
[`docs/a11y-contract.md` §4](../../../docs/a11y-contract.md). Summary:
panel is `role="dialog" aria-modal="true"` with `aria-label` derived
from the `title` prop; opens with focus moved into the panel (or to
`initialFocusRef`); Tab/Shift+Tab traps within the panel; Escape closes
(or pops the active child view first if one is up); background siblings
of the portal root are `inert` + `aria-hidden="true"` while open; focus
returns to the previously-focused element (or `restoreFocusRef`) on
close. The canonical contract spec lives in `docs/a11y-contract.md`;
this section deliberately does not duplicate it.

### Test

Selected via `getByRole('dialog', { name: title })`.
[`Dialog.test.tsx`](./Dialog/Dialog.test.tsx) covers the focus contract
(initial focus, Tab/Shift+Tab wrap, focus restore, inert siblings,
`initialFocusRef` precedence, `restoreFocusRef` precedence).
[`Dialog.contract.test.tsx`](./Dialog/Dialog.contract.test.tsx) covers
the `useModalView()` child-view stack: push replaces title + body but
keeps outer children mounted with `display: none`; pop returns the
outer title and leaves the pre-push focusables intact. Deferred to e2e:
the axe scan on the rendered dialog (lives in `web/e2e/a11y-axe.spec.ts`)
and visual regression of the panel chrome.

## ConfirmDialog

A small opinionated wrapper around [`Dialog`](./Dialog/Dialog.tsx) for
destructive-action confirmations (delete wave, remove cove, etc.).
Source: [`ConfirmDialog.tsx`](./ConfirmDialog/ConfirmDialog.tsx). Tests:
[`ConfirmDialog.test.tsx`](./ConfirmDialog/ConfirmDialog.test.tsx),
[`ConfirmDialog.contract.test.tsx`](./ConfirmDialog/ConfirmDialog.contract.test.tsx).
Added by PR
[#97](https://github.com/keanji-x/neige-calm/pull/97) (slice 1, PR3).

### Visual

Reuses Dialog's chrome (`.modal-overlay` / `.modal-panel` / `.modal-head`
/ `.modal-body`) verbatim — no new dialog CSS. The Confirm button uses
the existing `.go.warn` ruleset in `web/src/calm.css` when `destructive`
is true (the default), pairing the standard `.go` button shape with the
`--warn` token for color. Cancel uses `.go.outline`. The action row is
laid out with a small inline `display: flex; justify-content: flex-end;
gap` — no new class is introduced. Buttons sit Cancel-left, Confirm-right.

### Accessibility

Inherits the full a11y contract from Dialog: `role="dialog"
aria-modal="true"`, accessible name from the `title` prop, focus trap on
Tab/Shift+Tab, Escape closes, focus restored to the previously-focused
element on close, background siblings inerted. See
[`docs/a11y-contract.md` §4](../../../docs/a11y-contract.md) for the
canonical spec.

ConfirmDialog adds **one** primitive-specific rule on top:

- **Cancel-safe default focus.** When the dialog opens, focus lands on
  the Cancel button — never Confirm. A user mashing Enter on the dialog
  appearing dismisses it, not the destructive action behind it.
  Implemented by handing Dialog its `initialFocusRef` pointed at Cancel.

The primitive also wires Dialog's `onClose` to the caller's `onCancel`,
so Esc, overlay click, and the header X all route through `onCancel`.
Call sites get one dismissal callback, not two.

### Test

Selected via `getByRole('dialog', { name: title })`; the action buttons
via `getByRole('button', { name: 'Cancel' | 'Confirm' })` (or the
caller's custom label).
[`ConfirmDialog.test.tsx`](./ConfirmDialog/ConfirmDialog.test.tsx)
covers rendering: title + description, default and custom button
labels, the `go warn` class on Confirm when destructive (and its
absence when `destructive={false}`), and that nothing renders when
`open={false}`.
[`ConfirmDialog.contract.test.tsx`](./ConfirmDialog/ConfirmDialog.contract.test.tsx)
locks the six behavioral guarantees: (A) default focus on Cancel; (B)
Esc → `onCancel`; (C) overlay click → `onCancel`; (D) Enter on focused
Cancel → `onCancel`; (E) Enter on focused Confirm → `onConfirm`; (F)
rapid-Enter on initial focus never reaches `onConfirm`. Deferred to e2e:
the axe scan on the rendered dialog (already covered for Dialog in
`web/e2e/a11y-axe.spec.ts`) and visual regression of the warn-button
treatment.

### Adoption

ConfirmDialog SHOULD be used for any destructive action in the app —
deleting a wave, removing a cove, dropping a card, anything that
mutates state irreversibly. The contract makes Enter-on-open a no-op,
which `window.confirm()` does not.

Migrated flows (slice 1, PR #97 followup):

- **Delete cove (CovePage header)** —
  [`pages/_shared.tsx`](../pages/_shared.tsx) (`DeleteButton`), used at
  [`pages/Cove.tsx`](../pages/Cove.tsx). Pattern A: stay-open-while-pending —
  the dialog stays mounted during the async `onDelete`, with the
  Confirm button disabled via `confirmDisabled` so a second click or
  Enter can't re-fire the delete. Cancel remains enabled mid-await.
- **Delete wave (WavePage header)** —
  same `DeleteButton`, used at [`pages/Wave.tsx`](../pages/Wave.tsx).
  Inherits Pattern A from DeleteButton.
- **Delete wave (CovePage per-row ×)** —
  [`pages/Cove.tsx`](../pages/Cove.tsx) (`pendingDeleteWave` state +
  one page-level `<ConfirmDialog>` driven by all three `WaveRow`
  delete affordances). Pattern B: close-then-await — the dialog
  closes on Confirm and the parent's promise resolves out-of-band.
  The wave row vanishing from the list on completion is the
  user-visible "succeeded" signal.

New destructive flows added after this point MUST use ConfirmDialog
rather than `window.confirm` or an ad hoc inline confirmation. The
`confirmDisabled` prop (added alongside the DeleteButton migration)
is the canonical way to support an in-flight async confirm without
giving up the Cancel-safe default contract.

## Menu

The popover-menu primitive — a button trigger that opens a list of
menuitems. Source: [`Menu.tsx`](./Menu/Menu.tsx). Tests:
[`Menu.test.tsx`](./Menu/Menu.test.tsx),
[`Menu.contract.test.tsx`](./Menu/Menu.contract.test.tsx). Extracted from
`shared/components/AddPanel` in PR [#99](https://github.com/keanji-x/neige-calm/pull/99) (slice 2 of #60); AddPanel is
now a thin wrapper that maps registry entries to `MenuItem`s and
forwards selection back to its caller. The shared in-composite key
handling (`useRovingTabindex`) lives at
[`ui/hooks/useRovingTabindex.ts`](./hooks/useRovingTabindex.ts) and is
unit-tested independently in `useRovingTabindex.test.tsx`.

### Visual

Reuses the existing AddPanel rulesets in `web/src/calm.css` —
`.add-panel-wrap`, `.add-panel`, `.add-panel-menu`,
`.add-panel-menu-item`, `.add-panel-empty`. The Menu primitive itself
does NOT hard-code class names; callers pass `wrapClassName`,
`menuClassName`, `itemClassName`, and `emptyClassName`, and the active
(roving-focused) item gets `is-active` appended. Class-name renaming
to primitive-neutral selectors is a deliberate follow-up (the current
AddPanel-prefixed names are kept verbatim across the extraction so no
CSS diff lands in this PR).

### Accessibility

Implements the menu pattern documented in
[`docs/a11y-contract.md`](../../../docs/a11y-contract.md). Trigger is
a `<button>` (caller-owned) carrying `aria-haspopup="menu"` +
`aria-expanded`; the popover is `<ul role="menu">` with each entry as
`<button role="menuitem">`. Keyboard semantics come from
`useRovingTabindex` (ArrowUp/Down with wrap, Home/End, single-letter
typeahead with ~500ms idle reset, Enter/Space activate, Escape close);
the canonical contract for those keys lives in the hook. The Menu owns
the open/close lifecycle and the focus-restore policy.

Two contracts here are Neige-specific (not standard WAI-ARIA) and
worth calling out:

1. **Synchronous focus restore BEFORE `onSelect`.** When the user
   activates a menuitem, the Menu moves focus back to the trigger
   button *synchronously*, then calls `onSelect`. This ordering is
   load-bearing: if `onSelect` opens a Dialog, the Dialog's mount-time
   "snapshot the previously-focused element" effect must see the
   trigger as `document.activeElement` — otherwise the Dialog would
   snapshot the about-to-unmount menuitem and its close-time restore
   would noop. Locked by `Menu.contract.test.tsx`.

2. **Outside-click closes WITHOUT focus restore.** Dismissing the menu
   by clicking elsewhere on the page does not pull focus back to the
   trigger. The user gestured elsewhere intentionally, and yanking
   their focus away from the clicked target would be hostile.
   Symmetric with the Dialog overlay-click dismissal. Locked by
   `Menu.contract.test.tsx`.

### Test

Selected via `getByRole('button', { name })` (trigger) and
`getByRole('menuitem', { name })` (items) per a11y-contract §8.1.
[`Menu.test.tsx`](./Menu/Menu.test.tsx) covers the wiring layer: trigger
ARIA, popover structure, one representative key path per category
(ArrowDown, Home/End, typeahead, Escape, Enter activation), the empty
state, and disabled-item activation skip. It deliberately does NOT
re-prove the hook's behavior; that's
[`ui/hooks/useRovingTabindex.test.tsx`](./hooks/useRovingTabindex.test.tsx).
[`Menu.contract.test.tsx`](./Menu/Menu.contract.test.tsx) covers the
two Neige-specific contracts above end-to-end (the second test
exercises Menu + Dialog together so the Dialog's
previously-focused-element snapshot is part of the assertion).
Deferred to e2e: the axe scan on the open menu (lives in
`web/e2e/a11y-axe.spec.ts`) and real-browser keyboard traversal
(`web/e2e/a11y-keyboard.spec.ts`).
