# `app/shell`

The layout every route renders inside: the workspace rail plus the matched
route's outlet.

## Visual contract

`shell.module.css`, `@layer features` — app composition sits at the same cascade
position as the features it wraps (see the comment in
`tools/styles/repository-check.mjs`). Cove colour arrives as inline `style`
because it is per-row data.

## Accessibility contract

- The rail is a `<nav aria-label="Workspace">`; each section has an `<h2>`.
- Rows are `<button>` with `aria-current="page"` when their URL is open.
- **Intentionally not done:** no skip-to-main link (INV-A11Y-058). The rail is
  short and this has never been raised as a pain point; re-evaluate if a second
  long section lands. "There is no skip link" is a decision, not a defect.
- **Intentionally not done:** no `<a href>` navigation (INV-A11Y-061).

## Test contract

`getByRole`. `sidebar.contract.test.tsx` locks the invariants below; every one of
them was mutation-verified (break the production line, watch the named test go
red) before landing.

## Deliberate gaps

- **INV-SIDEBAR-007** — sections render **Waiting on you → Pinned → Coves**, and
  **pinning is not relocation**: a pinned wave appears under Pinned *and* in its
  cove's list, and if it also needs attention it appears in all three.
- **E2E-INV-SHELL-003** — the kernel system cove must never reach the rail. The
  server filters it; `coveListQueryOptions` filters it again. A fresh workspace
  renders zero cove rows.
- **INV-SIDEBAR-012 is not in this slice.** The rail is read-only until wave
  mutations land, so there is no pin button yet. When one lands it shows on hover
  *and* stays permanently visible once the wave is pinned — touch has no hover.
