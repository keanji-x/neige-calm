# #573 — fix a11y keyboard test 618 menu-focus regression

## Symptom

CI `web (build + test + a11y)` fails ONLY on `e2e/a11y-keyboard.spec.ts:618` ("Wave: toggle to list view, reorder with Alt+Arrow"). The test:

1. Clicks Atlas cove → Today wave.
2. Loop x2: focus `+ Add`, press Enter (menu opens), expect `getByRole('menuitem', {name: /terminal/i}).first()` to be focused.

The `toBeFocused()` assertion fails at iteration 0 with 14 retries all seeing `<button tabindex="-1" role="menuitem">` for Terminal.

Page screenshot (`/tmp/a11y-r4/test-results/...keybo-ad479*/test-failed-1.png`) shows the menu IS open with order TERMINAL, CODEX, CLAUDE, FILE, WEB PAGE — but **CODEX is the active/focused item** (index 1) instead of TERMINAL (index 0). The directAddError banner is also visible top-right ("internal: internal: proc-supervisor socket is not configured") because a prior iteration's terminal create failed.

Note: this test passes on `origin/main` and on this branch's earlier commits before commit `0b905d19` (which restored error visibility + report.md cache invalidation). The other 4 previously-failing keyboard tests + the axe contrast test all pass now after commit `00bbed70` (this fix-loop's contrast + clear-on-attempt fix). Only test 618 remains.

## Suspect

`web/src/shared/components/AddPanel.tsx:72` uses `<Menu items={menuItems} initialIndex={0}>`. The Menu primitive's `initialIndex: 0` should auto-focus the first item on open. Test comment at `a11y-keyboard.spec.ts:667` says "Terminal is registered first in registerBuiltins (cards/builtins/index.ts), so it's already focused".

Something in this PR causes Menu's roving focus to land on index 1 instead of 0. Possibilities (verify, don't guess):

- AddPanel's `menuItems` array memo invalidation flips order under re-renders driven by my new `directAddError` state in `web/src/pages/Wave.tsx:106` or report-sidebar's React Query churn.
- Menu's effect ordering changes when WavePage's React tree re-renders during the click → menu open transition because of `useQueryClient` added in `web/src/cards/builtins/wave-report.tsx:467` triggering subscriptions.
- The `<p role="alert" className="schema-form-error wave-add-direct-error">` element (rendered when directAddError is set) shifts focus order somehow.

## Investigate (READ-ONLY first, then propose + apply minimal fix)

Read these slices ONLY (do not grep the whole codebase):

- `web/src/shared/components/AddPanel.tsx:72` (Menu wiring)
- `web/src/ui/Menu/Menu.tsx` (initialIndex handling, focus effect, item-disabled detection)
- `web/src/cards/registry.ts:337` (`addPanelEntries`)
- `web/src/cards/builtins/index.ts` (register order)
- `web/src/pages/Wave.tsx` (my recent changes — directAddError + modalError state)
- `web/src/cards/builtins/wave-report.tsx` (my useQueryClient hook)
- `web/e2e/a11y-keyboard.spec.ts:618-679` (test 618)

Reproduce the page state from the screenshot if helpful (don't dispatch a browser — read code only). Common culprit pattern: Menu primitive auto-skips "disabled" items; if Codex registry entry now reports as enabled / Terminal as disabled under some condition, Menu lands on Codex.

## Fix

Apply the smallest fix that makes test 618 pass without breaking:
- The user-visible behavior that error surfaces inline (don't revert directAddError).
- The other 4 keyboard tests (currently passing).
- The axe color-contrast pass.
- All unit tests (`npm test` should still be 820 passing).

Acceptable fix shapes:
- Tweak Menu's `initialIndex` logic / autofocus effect.
- Adjust how / when directAddError renders so it doesn't disrupt Menu mount.
- Update the AddPanel registration so index 0 is stable.

If the right fix is to update test 618 itself (e.g. clear directAddError before reuse, or use a different way to wait for menu focus), document why and apply that.

## Gates

- `cd web && PATH=$HOME/.nvm/versions/node/v24.4.1/bin:$PATH npm test` (820 pass)
- `cd web && PATH=$HOME/.nvm/versions/node/v24.4.1/bin:$PATH npm run lint`
- `cd web && PATH=$HOME/.nvm/versions/node/v24.4.1/bin:$PATH npm run typecheck`

Write `docs/_impl-573-a11y-keyboard.md` (≤50 lines): root cause + fix + diff scope.

No grep -r. Use file:line slices. Do NOT run playwright/a11y locally (replay binary lacks proc-supervisor; that's the underlying limitation we're working around).
