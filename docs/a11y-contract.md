# Accessibility Contract — Neige Calm

**Status:** Living document. Anchored on the slices of [#56](https://github.com/keanji-x/neige-calm/issues/56) that have shipped (1–5). Update as the next slices land.
**Audience:** developers (and AI agents) touching the Neige frontend. Assumes a working understanding of `role`, `aria-label`, focus management, and Playwright's role-locator queries. Not a WAI-ARIA primer.
**Scope:** the `web/` package — the React UI rendered against `calm-server`. Server-side a11y concerns (e.g. SSE / WS shapes) are upstream of this doc; the rules below are about what reaches the DOM and the keyboard.

---

## 1. Why this contract exists

Neige is meant to be drivable by AI agents — and that puts an unusual amount of weight on stable `role` + accessible-name pairs. Two consumers care:

1. **Assistive tech.** A screen-reader user navigating a wave should hear consistent labels for "the cove name", "this card's status", "the rename affordance". Roles must match what the widget *does*, not what its CSS implies.
2. **AI agents driving the UI.** Our Playwright suite uses `getByRole(role, { name })` exclusively (no test IDs unless extraordinary). That's also the shape an external agent would use to drive the app: a brittle accessible name breaks both.

The Neige-specific angle layered on top of standard a11y: tests can pair UI assertions with **event-trace assertions** against `window.__neigeEvents__` (see §8). That lets us say "after clicking the role/name X, the event sequence Y happened" — a much stronger contract than either half alone. This is the principal reason this doc exists, and it's the principal reason role/name hygiene cannot be deferred: every test that asserts on UI semantics is also implicitly asserting on the locator we're documenting here.

Slices that have shipped against this contract:

- Slice 1 (#61): `jsx-a11y/recommended` ESLint baseline + cleanup.
- Slice 2 (#63): Modal focus trap, initial focus, focus restore, background inert.
- Slice 3 (#64): Implicit keyboard entry (Enter / F2) to rename.
- Slice 4 (#65): `prefers-reduced-motion` universal CSS override.
- Slice 5 (#66): Event-trace exposure (`window.__neigeEvents__`) + Playwright `a11y` project with `cargo run --bin replay --serve`.
- Slice 6 (#71): Keyboard-only E2E suite + axe scans + `npm run a11y*` scripts.
- Slice 7: AddPanel full menu keyboard semantics (arrow keys, Home/End, type-ahead, focus restore) via `useRovingTabindex`.
- Slice 8 (#67): this document.
- Slice 9: WaveGrid keyboard alternative — `WaveList` component, per-wave grid/list view-mode toggle persisted via overlay.

---

## 2. Neige object semantics

Each domain object in Neige has a recommended role/name pattern. When you add a new widget that touches an existing object kind, match the pattern below; if you're inventing a new object kind, add an entry here in the same PR.

### 2.1 Cove

A workspace cluster — the top-level grouping of work in the sidebar.

- **Sidebar Cove nav item**: rendered as a `<button>` whose accessible name is the cove name (the swatch and count are visual decoration, not part of the name). Source: `web/src/shared/components/Sidebar.tsx:62-77`. There is no `aria-label`; the visible text label inside `<span className="lbl">` *is* the accessible name, which is the right call — a screen reader reads "Atlas", not "Atlas 3" (we don't want the count count read as part of the name).
- **CovePage heading**: `<h1 className="h-display">` wrapping a `<button>` (when rename is available) or a plain `<h1>` (when it's read-only). The rename button carries `aria-label={value}` (the cove name) so heading-nav (`H` key in screen readers) announces e.g. "Atlas, heading level 1" without a "Rename cove name:" prefix. The rename verb is carried by a visually-hidden sibling `<span className="sr-only">` placed *outside* the `<h1>` and referenced via `aria-describedby`; AT verbalizes "Rename cove name" as the button's *description* when the button is focused. Source: `web/src/pages/Cove.tsx:286-313`. Arrow-into-actionable lands on the inner button.
- **New cove**: bootstrap-only sidebar affordance. Button with visible label "New cove". Source: `web/src/shared/components/Sidebar.tsx:132-141`.

### 2.2 Wave

A task thread / unit of work belonging to a cove.

- **Sidebar "waiting on you" row**: rendered as `<button className="side-wave">` with a `title` attribute holding "`<cove> · <title>`" and inline text equal to the wave title. Source: `web/src/shared/components/Sidebar.tsx:40-51`. The title attribute is informational only — the accessible name is just `<title>`.
- **WaveRow** (CovePage list): a `<div role="button">` with `tabIndex={0}` and an Enter/Space handler. Accessible name is computed from the inner text (wave title + optional working-count badge + optional now/eta strings). The per-row × is a child `<button aria-label="Delete \"<title>\"">` that stops propagation. Source: `web/src/shared/components/WaveRow.tsx:36-117`.
- **WavePage header crumb**: an `<h1>`-equivalent breadcrumb with the wave title rendered as `<span role="button" tabIndex={0} aria-label={wave.title} aria-describedby="<id>">` (sibling visually-hidden `<span className="sr-only">Rename wave</span>`) when rename is enabled, or a plain `<span>` when read-only. Same accessible-name split as CovePage's heading — name is just the title, the rename verb is the description. Source: `web/src/pages/Wave.tsx:209-273`.
- **Wave status pill**: `<span className="status-pill">` displaying the FSM verb ("Working", "Waiting on you", "Idle", ...). The `<CardStatusDot>` inside carries its own `aria-label="status <state>"` so the dot is announced even when the verb text is identical. Source: `web/src/pages/Wave.tsx:218-243` + `web/src/shared/components/CardStatusDot.tsx:48-79`.
- **View-mode toggle**: a `role="switch"` button in the wave-header `.wave-meta` cluster with `aria-checked={mode === 'list'}` and an accessible name of "Switch wave to list view" / "Switch wave to grid view". Source: `web/src/pages/Wave.tsx` (look for `.view-toggle`). Each wave persists its view mode independently via an overlay (`entity_kind: 'view'`, `kind: 'view-mode'`); see §2.7 for the overlay shape. Grid is the default for new waves so mouse-only users see no behavior change.
- **Two view modes**. WavePage renders one of two body components based on the per-wave view-mode overlay:
  - **Grid view** (default): `WaveGrid` from `web/src/WaveGrid.tsx`. RGL-powered, mouse-only for layout changes (drag via `.card-drag-handle`, resize via SE corner). Cards are individually Tab-reachable for their inner content; the layout itself has no keyboard story by design.
  - **List view** (Slice 9): `WaveList` from `web/src/WaveList.tsx`. Semantic `<ul>` of cards in `card.sort` order, with full keyboard navigation. **This is the keyboard-canonical mode** — keyboard users, screen-reader users, and AI agents driving the UI should switch a wave to list view to manipulate layout. The toggle is reachable from the wave-header and lives one Tab stop away from the AddPanel.

### 2.3 Card (generic)

A panel inside a wave. The kernel `KernelCard.kind` discriminates between subtypes; each kind has its own card component and its own role/name expectations. Common contract:

- Every card renders a header element with `className="card-drag-handle"`. This is the **only** part of the card that the grid treats as draggable (see `web/src/WaveGrid.tsx:405`), so anything outside the header is keyboard-focusable as expected without pointer-only interactions stealing keys. The header contains the card's accessible name (title text or kind-specific label).
- Cards do not declare `role="region"`. We may revisit this once we land per-card titled landmarks, but adding it today would announce "region" before every card title and degrade narration.
- Visual status uses `<CardStatusDot>` with its own accessible name (see §2.7).

### 2.4 Terminal card

Interactive PTY console.

- Outer container: `<div className="term">` (no role). The dragging header `<div className="term-head card-drag-handle">` holds the term title; live state appends "` · live`" to the title. Source: `web/src/cards/builtins/terminal.tsx:53-83`.
- xterm.js itself owns keyboard focus while the body is interacted with. We do not intercept keys at the React layer — the xterm renderer's own a11y story (which routes through a hidden textarea + live region) is the relevant contract from there. Source: `web/src/XtermView.tsx`.
- "terminal" is the fallback title when the kernel hasn't supplied a `card.title` yet. Don't rename — the test suite expects this literal.

### 2.5 Codex card

Agent workload with live FSM-driven status.

- Outer container: `<div className="codex-card">`. Header is `<div className="codex-card-head card-drag-handle">` with `<span className="codex-card-title">Codex</span>`.
- **Live status region**: `<div className="codex-status-bar" aria-live="polite">` wrapping the FSM verb + most-recent-hook label + a `<CardStatusDot>`. Source: `web/src/cards/builtins/codex.tsx:117-127`. `aria-live="polite"` (not assertive) on purpose — codex hooks can fire several times per second; assertive would interrupt other narration.
- Body: lazy `<XtermView>` once `card.terminalId` lands; otherwise placeholder text "Codex is starting… waiting for PTY." (`web/src/cards/builtins/codex.tsx:128-138`).

### 2.6 Plugin card (iframe)

Sandboxed plugin app rendered into an `<iframe>`.

- Outer container: `<div className="plugin-iframe-card">`. Header: `<div className="plugin-iframe-head card-drag-handle">` showing "Plugin: `<plugin_id>:<view_id>`". Source: `web/src/cards/plugin-iframe.tsx:283-294`.
- The `<iframe>` itself carries `title="plugin <plugin_id>/<view_id>"` (accessible name for the frame). Source: `web/src/cards/plugin-iframe.tsx:305`. `jsx-a11y/iframe-has-title` would fail without it; the chosen title doubles as the test locator.
- `sandbox="allow-scripts allow-same-origin"` is set; the plugin owns a11y *inside* the frame and is out of scope for this doc. The host contract is that the frame must be reachable via `getByTitle` from outside.

### 2.7 Overlay / status

Not a UI element — overlays are a **state mechanism**. The kernel publishes `overlay.set` events (status, layout, etc.) on per-entity topics; the UI surfaces them as visual artifacts. From an a11y standpoint:

- The 6-state card FSM surfaces as `<CardStatusDot>` (see `web/src/shared/components/CardStatusDot.tsx`) with `aria-label="status <state>"`. The same dot drives both per-card status bars and the wave-level glyph (`web/src/shared/components/WaveRow.tsx:50-58`), so the same accessible-name format reaches both surfaces.
- Codex's live-status string is surfaced via the `aria-live="polite"` region noted in §2.5. Other overlay kinds may want their own live region — match this pattern (polite, narrow scope) when adding one.
- **View-mode overlay** (Slice 9): per-wave preference for grid vs list layout. Persisted at `(plugin_id='kernel', entity_kind='view', entity_id=<waveId>, kind='view-mode')` with payload `{ schemaVersion: 1, mode: 'grid' | 'list' }`. Kept distinct from the existing `kind: 'layout'` overlay so list-mode users (who don't drag) never have to mint a layout row just to flip the toggle. New schema constant `OVERLAY_VIEW_MODE_SCHEMA_VERSION` lives alongside the layout one in `web/src/cards/builtins/schemaVersions.ts`. Kernel-side: no validator entry needed — unknown overlay kinds fall through the catch-all `_ => Ok(())` in `validate_overlay_payload`.

---

## 3. Keyboard contracts

### 3.1 Tab order

Pages should be Tab-traversable end-to-end. Concrete shape today:

- **Sidebar → main**: the sidebar renders a flat sequence of `<button>` elements, so Tab walks Today → waiting-on-you waves → coves → New cove → main content. No skip-link yet; the sidebar is short enough that this hasn't been raised as a pain point, but if we add another sidebar section we should reconsider.
- **CovePage**: title (rename button if available) → section rows (each `<div role="button">`) → `+ New wave` ghost button. Section headers are not tab stops.
- **WavePage**: back button → cove crumb button → wave title rename button → view-mode toggle → Add panel → delete button → cards in the grid (grid view) or list (list view). Cards' internal focus stops depend on the kind (xterm grabs focus once activated; plugin iframes own their own internal sequence).

Layout-change semantics (Slice 9): grid view is mouse-only for drag/resize by design; the per-wave view-mode toggle (one Tab stop before AddPanel) flips the wave to list view, where reorder is keyboard-driven via `Alt+ArrowUp` / `Alt+ArrowDown` on the focused row. See §3.4 for the full list-view contract.

### 3.2 Activation

- Native `<button>`: Enter and Space activate. Use a real button wherever possible.
- Element with `role="button"`: must handle Enter and Space explicitly. See WaveRow for the canonical pattern (`web/src/shared/components/WaveRow.tsx:42-48`).
- Native `<a href>` link: Enter activates. We don't have many links today; most navigation goes through `<button>` + `onGo`.
- **Rename pattern (Slice 3)**: Enter or F2 on the rename target enters edit mode. See `web/src/pages/Wave.tsx:189-198` and `web/src/pages/Cove.tsx:279-285`.

### 3.3 Escape

- **Modal**: closes the modal. If a child view is pushed, the child's `onEscape` runs first (or the view pops if no handler). See `web/src/shared/components/Modal.tsx:152-160`.
- **AddPanel popover**: closes the menu and returns focus to the trigger button. Owned by `useRovingTabindex`'s `onEscape` callback (`web/src/hooks/useRovingTabindex.ts`); AddPanel routes that callback through `closeAndRestoreFocus`.
- **Rename edit input**: cancels the edit, restoring focus to the display element. See `web/src/pages/Wave.tsx:138-141` and `web/src/pages/Cove.tsx:226-229`.
- **Inline NewWave / NewCove input**: blur-commits (the input's `onBlur` calls `submit()`); Escape is wired to `close` which dumps the draft.

### 3.4 Arrow keys

- **WaveGrid**: no keyboard reorder. By design — the grid stays mouse-only; keyboard users switch the wave to list view via the wave-header toggle.
- **WaveList** (Slice 9): the keyboard-canonical alternative. Each card is rendered inside an `<li>` participating in a roving tabindex (`useRovingTabindex`). Bindings:
  - **ArrowUp / ArrowDown** — move focus between cards (wraps).
  - **Home / End** — jump to first / last card.
  - **Alt+ArrowUp / Alt+ArrowDown** — reorder the focused card up / down by swapping `sort` values via `useUpdateCardMutation` (which is optimistic for `sort`). The trace ring buffer picks up the resulting `card.updated` events.
  - **Delete / Backspace** — remove the focused card (same as the `×` button; no confirmation, matching grid view's affordance).
  - **Tab** — exits the list to whatever follows; Shift+Tab returns to the wave-header.
  - Each `<li>` carries `aria-keyshortcuts="ArrowUp ArrowDown Alt+ArrowUp Alt+ArrowDown Home End Delete"` so the contract is discoverable from AT alone.
- **AddPanel menu** (Slice 7): full WAI-ARIA menu keyboard contract via `useRovingTabindex` (`web/src/hooks/useRovingTabindex.ts`). ArrowDown/Up cycle with wrap, Home/End jump, single-letter typeahead jumps to first match, Enter/Space activate, Escape closes. Roving `tabIndex` keeps the menu out of the Tab order — only the active item is in the page sequence. On open, the first item is focused; on close (Escape, activation, outside click), focus returns to the trigger.
- **xterm.js** owns arrow keys inside terminal/codex bodies — they're forwarded to the PTY.

---

## 4. Modal contract

Slice 2 (#63). The public API lives in `web/src/shared/components/Modal.tsx:41-56`:

```ts
interface ModalProps {
  open: boolean;
  onClose: () => void;
  title?: string;
  children?: React.ReactNode;
  wide?: boolean;
  initialFocusRef?: RefObject<HTMLElement | null>;
  restoreFocusRef?: RefObject<HTMLElement | null>;
}
```

Behavior contract while `open` is true:

1. **Initial focus.** One animation frame after open, focus moves into the panel. Resolution order: caller's `initialFocusRef.current` → first focusable inside the panel → the panel itself (which has `tabIndex={-1}` as a fallback). Source: `web/src/shared/components/Modal.tsx:174-200`.
2. **Focus trap.** Tab / Shift+Tab cycle inside the panel; reaching either end wraps to the other. The focusables list is re-queried on every Tab keydown so dynamic child content (e.g. a pushed view) is picked up automatically. Source: `web/src/shared/components/Modal.tsx:275-300`.
3. **Background inert.** Every direct child of `document.body` *except* the portal root gets `inert` + `aria-hidden="true"` while the modal is up; prior values are restored exactly on close. Source: `web/src/shared/components/Modal.tsx:220-252`.
4. **Focus restore.** On close, focus returns to `restoreFocusRef.current` if provided, else to whatever element had focus when the modal opened. Detached nodes are skipped silently. Source: `web/src/shared/components/Modal.tsx:201-212`.
5. **Escape.** Esc closes the modal. When a child view is up (via `useModalView().pushView(...)`), Esc goes to the view first (its `onEscape` handler), so a `DirectoryBrowser` can cancel its own browse-mode without closing the whole modal. Source: `web/src/shared/components/Modal.tsx:148-169`.
6. **Click outside.** Mousedown on the overlay (not the panel) closes — except while a child view is up, where overlay-click is disabled to prevent losing half-filled form state behind it. Source: `web/src/shared/components/Modal.tsx:313-321`.
7. **Role + name.** The panel is `role="dialog" aria-modal="true"` and uses the `title` prop (if string) as `aria-label`. Source: `web/src/shared/components/Modal.tsx:338-341`.

We deliberately do **not** use the platform `<dialog>` element. Cross-theme styling of the native dialog is unreasonable (UA defaults override our tokens); the cost of hand-rolling the focus trap is documented in the file header and reaffirmed in §10.

---

## 5. Rename contract

Slice 3 (#64). Two implementations: WavePage title (`web/src/pages/Wave.tsx:185-208`) and CovePage `EditableTitle` (`web/src/pages/Cove.tsx:188-293`). Both share these rules:

- **Display element is keyboard-focusable.** WavePage uses `<span role="button" tabIndex={0}>`; CovePage uses a real nested `<button>` inside the `<h1>` (preferred — intrinsic role). Both are first-class tab stops.
- **Accessible name is the value; the rename verb is the *description*.** WavePage: `aria-label={wave.title}` + `aria-describedby` → a sibling `<span className="sr-only">Rename wave</span>`. CovePage: `aria-label={value}` + `aria-describedby` → a sibling `<span className="sr-only">Rename {ariaLabel.toLowerCase()}</span>` placed **outside** the wrapping `<h1>` so the heading's own accessible-name computation isn't polluted by the helper. Net effect: heading-nav (`H` in screen readers) announces "Atlas, heading level 1" — not "Rename cove name: Atlas, heading level 1" — while AT still verbalizes "Rename cove name" as the button's description when the button is focused. This split is the canonical pattern for any new rename surface; reach for `aria-describedby` over stuffing the verb into the name. The visually-hidden helper uses the project-wide `.sr-only` class (`web/src/calm.css`).
- **Entry keys**: Enter or F2. F2 is the Windows convention and the only key explicitly preventDefault'd; Enter is the native button activation. Source: `web/src/pages/Wave.tsx:197-209` + `web/src/pages/Cove.tsx:299-308`.
- **Edit input behavior**: Enter commits (calls `onSave`), Escape cancels, blur commits (so click-elsewhere also saves). The input inherits the display element's visual class so it doesn't visually shift on swap.
- **Focus restore on exit.** Both implementations stash a ref to the display element and re-focus it after the `editing` flag flips back to false. The mechanism uses a `restoreFocus` boolean ref so the focus call happens after React unmounts the input. Source: `web/src/pages/Wave.tsx:115-126` + `web/src/pages/Cove.tsx:208-223`.

When adding a new rename surface, copy the CovePage pattern — the intrinsic `<button>` inside the heading is cleaner than the `role="button"` span and gives correct semantics with no hand-rolled key handling for the activation case. Apply the same name/description split: `aria-label` carries the value, `aria-describedby` points to an `sr-only` span carrying the verb.

---

## 6. Focus-visible policy

Slice 1 (#61) put `jsx-a11y/recommended` in the build (`web/eslint.config.js:100-105`). The lint catches most "missing accessible name", "missing role", etc. cases on JSX. It does NOT catch CSS-only `outline: none` regressions — that's manual hygiene.

Rules:

- **Never write a bare `outline: none`.** Every `outline: none` in `web/src/calm.css` must be paired with a `:focus-visible` rule that re-establishes a visible focus indicator. Compliance audit lives in `web/src/calm.css`; current pairings:
  - `.wave-title[role="button"]` (`calm.css:617-623`)
  - `.wave-title-input` (`calm.css:627-638`)
  - `.h-display-rename` (`calm.css:644-659`)
  - `.cove-title-input` (`calm.css:663-668`)
  - `.new-wave-input` (`calm.css:670-675`)
  - `.wave-back`, `.wave-cove`, `.crumb-link` use solid `outline` + `outline-offset` (no `outline: none` to pair against).
- **Use `var(--accent-soft)` for soft rings, `var(--accent)` for hard ones.** The "soft" form is a 2px `box-shadow` — visually quieter, used for inline-edit surfaces. The "hard" form is a 2px outline + 2px offset, used for chrome buttons. Both forms must be visible against the panel background; we don't dim them theme-side.
- **Don't rely on the global `*:focus { outline: none }`.** It doesn't exist (the calm reset is per-class). Adding one is forbidden — it would silently nuke browser default focus rings on every input we haven't styled.

If you add a new focusable element, the checklist is: (1) does it have a visible focus indicator out of the box? If yes, you're done. If no, (2) add a `:focus-visible` rule with one of the two patterns above.

---

## 7. Motion policy

Slice 4 (#65). The rule is universal: every animation and transition in the codebase is decorative; none signal load state or convey information through motion alone.

- **`prefers-reduced-motion: reduce`** collapses every CSS animation/transition to 0.01ms. Source: `web/src/calm.css:2483-2492`. The `!important` is required to win against any inline `animation:` shorthand.
- "Loading…" indicators use **text**, not spinners (e.g. `web/src/pages/Wave.tsx:256` — `<div className="synth">Loading grid…</div>`). Don't add a spinner where text would do.
- JS code in `web/src/` does **not** listen for `animationend` / `transitionend`. Collapsing duration to 0.01ms still fires those events synchronously enough that even if you accidentally add a listener, you'll be fine — but the contract is "decorative only".

**If you add a functional animation** (one where motion carries information — e.g. a spinner where the spin direction encodes load state), you must document the reduced-motion alternative in the same PR. Adding `animation:` to a new element is otherwise free; the universal override applies.

---

## 8. AI agent test contract

This is the contract test authors actually live in. **Read this section before writing a new Playwright spec.**

### 8.1 Locator rules

- **`getByRole(role, { name })` is the default.** This is the same path screen readers and AI agents use; if a test can't reach an element this way, neither can they.
- **Test IDs only in extraordinary cases.** "Extraordinary" means: the element legitimately has no role/name story (e.g. a purely decorative canvas) AND the test absolutely needs to assert on it. So far no slice has needed one.
- **`getByText` is acceptable for unambiguous body text** (e.g. the `Codex is starting…` placeholder). When the text appears inside an element with a role, prefer `getByRole`.
- **`getByTitle` is acceptable for the plugin iframe** — its accessible name *is* its title (see §2.6). Don't add a `title` attribute just to be queryable; `aria-label` is the right escape hatch elsewhere.

### 8.2 Event-trace assertions

The unique Neige angle. Under a dev build with `?trace=1` on the URL, `EventBridge` installs a 200-entry ring buffer at `window.__neigeEvents__` (`web/src/app/eventBridge.tsx:64-146`). Tests can pair UI assertions with event-trace assertions to lock down *both* the visible state and the wire sequence that produced it.

Helpers live in `web/e2e/helpers/trace.ts`:

- `getEventTrace(page)` — snapshot the buffer.
- `clearEventTrace(page)` — empty in place (preserves cached refs in components).
- `assertEventKinds(page, expected)` — exact-sequence assertion on the `ev` field.
- `waitForEvent(page, kind, timeoutMs?)` — poll until a matching event lands; uses `page.waitForFunction` for sub-ms granularity.

### 8.3 Playwright projects

Two projects share `playwright.config.ts`:

- **`chromium`** — points at the developer's `make dev` stack (`http://localhost:4040/calm/`). Used for `golden-path.spec.ts`, `wave-create.spec.ts`. No replay binary needed.
- **`a11y`** — boots `cargo run --bin replay --serve` (Slice 5, see `web/e2e/_setup/replay-server.ts`) preloaded with a curated event-trace fixture. Use this project for any spec that needs the event trace ring buffer. Reference impl: `web/e2e/a11y-trace-smoke.spec.ts`.

The replay binary is spawned exclusively by the `replay-setup` setup project, which only runs as a dependency of `a11y`. Running `--project=chromium` alone never needs cargo on PATH.

### 8.4 Test naming

Tests for a11y / role-name contracts go under `web/e2e/`. Tests that touch the replay fixture must use the `a11y` project. The convention is one spec per surface (one for the modal contract, one for the rename contract, etc.); cross-surface coverage is fine to scope inside a single `describe`.

---

## 9. Deferred / known gaps

Catalogued so a maintainer reading this doc doesn't think the gap is undiscovered.

- ~~**AddPanel full menu keyboard semantics (Slice 7, pending).**~~ **Resolved** by Slice 7 — see §3.4 above and `web/src/hooks/useRovingTabindex.ts`.
- ~~**WaveGrid keyboard reorder/resize (Slice 9, deferred).**~~ **Resolved** by Slice 9, **via Path C** (separate list-view component) — see §2.2 "Two view modes" and §3.4 "WaveList". Grid view itself remains mouse-only by design; keyboard / AT users flip the per-wave view-mode toggle to switch to list view, which is the keyboard-canonical mode. List view supports reorder (`Alt+ArrowUp` / `Alt+ArrowDown` → `card.sort` swap via the existing optimistic mutation) and remove (`Delete`); resize is out of scope (cards in list view self-size to intrinsic content).
- ~~**Heading-nav narration noise on rename buttons.**~~ **Resolved** (#56 followup): both WavePage's title span and CovePage's `EditableTitle` now carry `aria-label={value}` only; the rename verb is moved to a visually-hidden sibling span referenced via `aria-describedby`. Heading-nav (`H` in screen readers) now announces "Atlas, heading level 1" without a "Rename cove name:" prefix while AT still verbalizes the action as the button's description when focused. See §5 for the canonical pattern.
- **`Modal.tsx:111-117` comment about `:focus-visible` filtering.** Captured during Slice 2 review — the comment is technically incorrect (it claims `:focus-visible` matches against display:none, which it doesn't in practice). Latent fragility if DOM order shifts but no behavior bug today. Worth a follow-up cleanup pass.
- **Sidebar skip-link.** No skip-to-main link today. Sidebar is short enough that it hasn't been raised; reconsider if a sidebar section grows large.
- **xterm.js inner a11y.** Out of scope — the renderer owns its own contract. We don't intercept keys at the React boundary.

---

## 10. When to introduce a headless UI library

The current stance: **hand-roll until pain forces a library**. The Modal focus trap (Slice 2) was the borderline case — hand-rolling it took ~150 lines and one round of review feedback, which is roughly the threshold for "one more of these and we should reconsider".

Triggers for revisiting:

1. The disclosure-widget inventory grows past ~5 widgets in active maintenance. Today we have Modal + AddPanel popover (with full menu semantics post-Slice 7); that's still well under 5.
2. A new widget category lands that needs keyboard semantics we can't reasonably hand-roll. Combobox is the most likely candidate (autocomplete + arrow keys + value binding is hard to get right without a reference impl). A command palette would be another.
3. Two or more existing hand-rolled widgets drift apart in their focus-management code — i.e. we've forked the same logic and it's diverged. Today the only shared logic is "focus restore on close", duplicated across Modal, the rename surfaces, and the AddPanel popover in a way that's still readable.

Candidates evaluated when this question comes up:

- **Radix UI Primitives** — most likely choice. Unstyled, headless, well-tested focus management. Pulls in zero design tokens.
- **Headless UI** — Tailwind-coupled mental model even though the library itself is style-agnostic; smaller surface area than Radix.
- **React Aria** — most rigorous a11y-wise but its component shape doesn't always match our preferences (a lot of "use this hook" instead of "render this component").
- **Ark UI** — newer, Zag-machine-based; appealing but less battle-tested in production.

**Slice 7 verdict — stay hand-rolled.** The full WAI-ARIA menu keyboard contract (arrow keys + Home/End + typeahead + roving tabindex + focus restore) fit in ~290 LOC of hook + ~30 LOC of integration. Painful edge cases were one (1): React 19 StrictMode double-effect surfacing a latent effect-ordering bug in Modal's inert blanket vs focus restore (fixed by re-declaring the inert effect before the focus effect — see `Modal.tsx`). No round of review feedback was burned on the hook shape itself. The library question remains "no" — re-evaluate at the next disclosure-widget addition.

---

## 10a. Primitive layer

Per-primitive contracts (visual, accessibility, test) live in [`web/src/ui/README.md`](../web/src/ui/README.md), introduced by [#60](https://github.com/keanji-x/neige-calm/issues/60). That README is the canonical home for the contract of each primitive in `web/src/ui/` (currently `Dialog`; `Menu` and `ConfirmDialog` follow in subsequent slices). This document remains the canonical home for cross-cutting rules (object semantics, keyboard contracts, locator rules, motion policy) — primitive-specific contracts are intentionally not duplicated here.

---

## 11. References

- Issue [#56 — frontend a11y contracts](https://github.com/keanji-x/neige-calm/issues/56). Top-level issue this work hangs off.
- Slice PRs:
  - #61 — jsx-a11y baseline.
  - #63 — Modal focus contract.
  - #64 — Rename keyboard entry.
  - #65 — `prefers-reduced-motion`.
  - #66 — Event-trace exposure + Playwright `a11y` project.
  - #67 — this document (Slice 8).
  - #71 — Slice 6, Keyboard E2E + axe scans + `npm run a11y*` scripts.
  - #73 — Slice 7, AddPanel menu keyboard semantics + `useRovingTabindex`.
  - Slice 9 — WaveGrid keyboard alternative (`WaveList` + per-wave view-mode toggle).
- `web/playwright.config.ts` — top-of-file comment documents the two-project layout.
- `web/e2e/README.md` — running the test suites locally.
- `web/e2e/helpers/trace.ts` — the event-trace helper API used by `a11y` specs.
- Multica's frontend test suite uses `getByRole(role, { name })` heavily; we share that selector discipline. We do **not** share their product semantics — Cove / Wave / Card / Terminal-card / Codex-card / Plugin-card are Neige primitives and the role/name patterns above are specific to them.
