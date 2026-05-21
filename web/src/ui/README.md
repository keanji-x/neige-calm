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

## Reserved

Stub headings for upcoming primitives. Filled in by the PRs noted below.

### Menu

TODO: filled in by PR #XXX (slice 2, Menu extraction from `AddPanel`).

### ConfirmDialog

TODO: filled in by PR #XXX (slice 3, ConfirmDialog primitive).
