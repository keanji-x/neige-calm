// Axe-core scans for each major page + a couple of "open transient state"
// snapshots (modal up, AddPanel menu up). The point of these is to catch
// the bucket of things role/name keyboard tests can't see — color
// contrast, label/control association, landmark structure, etc.
//
// Coverage matrix:
//   Page                              | Spec name                       |
//   -----------------------------------|---------------------------------|
//   /calm/ (Today)                    | "Today page · axe scan"         |
//   /calm/cove/<id>                   | "Cove page · axe scan"          |
//   /calm/wave/<id>                   | "Wave page · axe scan"          |
//   /calm/settings                    | "Settings page · axe scan"      |
//   Wave + AddPanel menu open         | "AddPanel open · axe scan"      |
//   Wave + Modal open                 | "Modal open · axe scan"         |
//
// Runs under the Playwright `a11y` project (so it talks to a Vite dev
// server fronting the in-process replay binary). Same constraint as
// `a11y-keyboard.spec.ts`: requires `cargo` on PATH and the `replay`
// binary to be buildable.
//
// We deliberately don't blanket-disable any rule. If a third-party
// component fails a check, the right move is to call it out in the
// finding (a comment on the failing spec) and decide whether to fix or
// defer. The "common" pages (Today, Cove, Wave, Settings) MUST come out
// clean — if axe ever turns up violations on those, fix the source, don't
// silence the spec.

import { test, expect, type Page } from '@playwright/test';
import { AxeBuilder } from '@axe-core/playwright';
import { resetReplayServer } from './helpers/reset';

// Wait for the auto-bootstrap to land. `useTodayTerminal` runs on first
// paint of the Today page and creates "Scratch / Today / <terminal>"
// when the kernel has no state — which is the replay binary's starting
// position (events seeded, entity tables empty). The Sidebar's "Scratch"
// button is the stable end-state of bootstrap; we anchor on it because
// the trace buffer doesn't backfill events that fired before the WS
// connect, so we can't anchor on the fixture's `overlay.set` directly.
async function waitForBootstrap(page: Page): Promise<void> {
  await expect(
    page.locator('aside.side').getByRole('button', { name: /scratch/i }),
  ).toBeVisible({ timeout: 15_000 });
}

// Flip the app into dark mode for the parallel dark-theme scans. The
// theme is React state living in CalmApp (no localStorage) — the
// effect mirrors `theme` into `document.documentElement.dataset.theme`.
// We can't invoke the React setter from outside the bundle, but the
// dataset attribute is exactly what the `[data-theme="dark"]` selectors
// in `calm.css` consume — so writing it directly is sufficient to
// re-paint the cascade for axe's color-contrast probe. We wait on a
// `waitForFunction` checking the attribute *and* the computed background
// color of <body> (which should darken once the cascade re-evaluates)
// so the scan never races a half-applied theme. Note: the next user
// action that triggers CalmApp's effect will overwrite our attribute,
// which is fine — the scan runs to completion before any such action.
async function enableDarkTheme(page: Page): Promise<void> {
  await page.evaluate(() => {
    document.documentElement.dataset.theme = 'dark';
  });
  await page.waitForFunction(() => {
    if (document.documentElement.dataset.theme !== 'dark') return false;
    // --bg in dark mode is oklch(16% …), which resolves well below
    // rgb(128,128,128). Light mode --bg sits near rgb(252,252,253).
    // Sampling <body>'s computed background-color gives us a stable
    // post-cascade signal that the CSS variables actually flipped.
    const bg = getComputedStyle(document.body).backgroundColor;
    const m = bg.match(/\d+/g);
    if (!m) return false;
    const [r, g, b] = m.map(Number);
    return r < 80 && g < 80 && b < 80;
  });
}

// Rules disabled across all scans below. No rules currently deferred;
// add here with a comment block explaining why a rule had to be
// silenced and what the follow-up plan is. The empty array still
// flows through `disableRules(...)` below so the wiring stays
// discoverable for the next time we need to defer something.
//
// Notable resolved rules (kept here for archaeology):
//   - region (PR #122): TitleBar promoted to `<header>` so the chrome
//     sits inside an implicit `banner` landmark.
//   - nested-interactive (PR #127): fixed by the WaveRow refactor — the
//     row is now a real `<button>` with a sibling delete `<button>`
//     inside a `.wave-row-wrapper`.
//   - color-contrast (this PR): --text-3 (light + dark) and --accent
//     (light) bumped to clear ≥ 4.5:1 on every observed background
//     surface; .nav-label re-routed from --text-4 to --text-2.
const DEFERRED_RULES: string[] = [];

// Default Axe builder used by every scan. `withTags` pins the rule set to
// WCAG 2.1 A + AA + best-practice; we don't want a future axe-core
// release to silently surface AAA-only checks and turn the suite red.
function axe(page: Page): AxeBuilder {
  return new AxeBuilder({ page })
    .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'best-practice'])
    .disableRules(DEFERRED_RULES);
}

// Pretty-print axe violations so a failure surfaces the rule id + impact
// + element selectors in the report rather than [Object object]. Returns
// an empty string when there are no violations so the assertion message
// stays clean.
function formatViolations(
  violations: {
    id: string;
    impact?: string | null;
    help: string;
    nodes: { target: unknown[] }[];
  }[],
): string {
  if (violations.length === 0) return '';
  return violations
    .map(
      (v) =>
        `[${v.impact ?? 'unknown'}] ${v.id} — ${v.help}\n  nodes: ${v.nodes
          .map((n) => JSON.stringify(n.target))
          .join(', ')}`,
    )
    .join('\n');
}

// Resolve the auto-created Scratch cove + Today wave ids from the live
// DOM rather than hard-coding them — they're kernel-generated UUIDs that
// vary per run. We click sidebar / wave-row affordances directly here
// (not keyboard-only) because this helper is just plumbing for the axe
// scans; the keyboard-only contract lives in `a11y-keyboard.spec.ts`.
//
// The locator scoping matters: the Sidebar has its own "Today" button
// (the nav-item back to /), and the cove page renders a WaveRow whose
// title is also "Today" (the auto-bootstrapped wave). With the WaveRow
// real-button refactor (#56/#60 follow-up) all three are now real
// <button>s sharing the name "Today" — so we scope the wave row by the
// `<section aria-label="Waves">` landmark that CovePage wraps around
// the wave lists. See §2.2 of `docs/a11y-contract.md`.
async function ids(page: Page): Promise<{ coveId: string; waveId: string }> {
  await page.goto('/?trace=1');
  await waitForBootstrap(page);
  await page
    .locator('aside.side')
    .getByRole('button', { name: /scratch/i })
    .click();
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+(\?|$)/);
  const coveId = new URL(page.url()).pathname.split('/').pop()!;
  // WaveRow is now a real <button> (see WaveRow.tsx). The "Waves"
  // region landmark on CovePage lets us scope past the colliding
  // sidebar Today nav button and the Crumbs Today link without
  // resorting to DOM-tag selectors.
  await page
    .getByRole('region', { name: 'Waves' })
    .getByRole('button', { name: /today/i })
    .first()
    .click();
  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+(\?|$)/);
  const waveId = new URL(page.url()).pathname.split('/').pop()!;
  return { coveId, waveId };
}

test.describe('a11y · axe', () => {
  test.beforeEach(async ({ request }) => {
    // Hermetic per-test state — see `helpers/reset.ts`. Axe scans don't
    // mutate state themselves, but `ids(page)` clicks through "+ Add" /
    // codex modals in some tests and we don't want their residue (extra
    // cards, opened modals' overlay payloads) leaking into the next
    // spec's DOM.
    await resetReplayServer(request);
  });

  test('Today page · no violations', async ({ page }) => {
    await page.goto('/?trace=1');
    await waitForBootstrap(page);
    const { violations } = await axe(page).analyze();
    expect(violations, formatViolations(violations)).toEqual([]);
  });

  test('Cove page · no violations', async ({ page }) => {
    const { coveId } = await ids(page);
    await page.goto(`/calm/cove/${coveId}?trace=1`);
    await waitForBootstrap(page);
    // CovePage paints its header (h1, eyebrow, …) synchronously once
    // covesQuery resolves. Wait for the title before scanning so we
    // don't catch a half-rendered skeleton.
    await expect(page.getByRole('heading', { name: /scratch/i })).toBeVisible();
    const { violations } = await axe(page).analyze();
    expect(violations, formatViolations(violations)).toEqual([]);
  });

  test('Wave page · no violations', async ({ page }) => {
    const { waveId } = await ids(page);
    await page.goto(`/calm/wave/${waveId}?trace=1`);
    await waitForBootstrap(page);
    // WaveGrid is lazy-loaded — wait for AddPanel to render before
    // scanning so the wave page's full role tree is in the DOM.
    await expect(page.getByRole('button', { name: /\+\s*add/i })).toBeVisible();
    const { violations } = await axe(page).analyze();
    expect(violations, formatViolations(violations)).toEqual([]);
  });

  test('Settings page · no violations', async ({ page }) => {
    await page.goto('/calm/settings?trace=1');
    // The form mounts with empty/default values; we still wait for the
    // first input to be present so the scan covers the real DOM, not a
    // pre-hydration shell.
    await expect(page.getByRole('textbox', { name: /http proxy/i })).toBeVisible({
      timeout: 15_000,
    });
    const { violations } = await axe(page).analyze();
    expect(violations, formatViolations(violations)).toEqual([]);
  });

  test('AddPanel open · no violations on menu', async ({ page }) => {
    const { waveId } = await ids(page);
    await page.goto(`/calm/wave/${waveId}?trace=1`);
    await waitForBootstrap(page);
    // Open the menu via keyboard so we're scanning the same transient
    // state a real user would land in. Slice 7 may rework the menu's
    // keyboard semantics but the open-on-Enter contract holds today.
    const trigger = page.getByRole('button', { name: /\+\s*add/i });
    await expect(trigger).toBeVisible();
    await trigger.focus();
    await page.keyboard.press('Enter');
    await expect(page.getByRole('menu')).toBeVisible();
    // Scope the scan to the menu region — scanning the whole document
    // would re-flag everything from the page-level scan above. We
    // explicitly want "is the menu itself ARIA-clean?".
    const { violations } = await axe(page).include('[role="menu"]').analyze();
    expect(violations, formatViolations(violations)).toEqual([]);
  });

  // Slice 9: the list-view alternative to WaveGrid. Same role/name
  // hygiene applies — labels, roles, landmark structure should come
  // out clean. The role="switch" toggle is the new control we want
  // covered along with the list itself.
  test('Wave list view · no violations', async ({ page }) => {
    const { waveId } = await ids(page);
    await page.goto(`/calm/wave/${waveId}?trace=1`);
    await waitForBootstrap(page);
    // Wait for the wave page to fully render before flipping the
    // toggle — WaveGrid is lazy-loaded.
    await expect(page.getByRole('button', { name: /\+\s*add/i })).toBeVisible();
    const toggle = page.getByRole('switch', { name: /switch wave to list view/i });
    await expect(toggle).toBeVisible();
    await toggle.click();
    // List mode lazily mounts; wait for the <ul> before the scan.
    await expect(page.getByRole('list', { name: /wave cards/i })).toBeVisible({
      timeout: 5_000,
    });
    const { violations } = await axe(page).analyze();
    expect(violations, formatViolations(violations)).toEqual([]);
  });

  test('Modal open · no violations on dialog', async ({ page }) => {
    const { waveId } = await ids(page);
    await page.goto(`/calm/wave/${waveId}?trace=1`);
    await waitForBootstrap(page);
    // Same path as the keyboard spec: open AddPanel, pick the codex
    // menuitem (the only built-in with a createSchema → modal).
    const trigger = page.getByRole('button', { name: /\+\s*add/i });
    await trigger.focus();
    await page.keyboard.press('Enter');
    const codexItem = page.getByRole('menuitem', { name: /codex/i });
    const hasCodex = (await codexItem.count()) > 0;
    test.skip(!hasCodex, 'codex card kind not registered in this fixture');
    // Slice 7's roving-tabindex menu: ArrowDown from the (focused) first
    // menuitem to land on codex, then Enter activates *that* item.
    // `codexItem.press('Enter')` would fire keydown on the codex button
    // but the hook reads its internal `activeIndex` to decide which item
    // to activate — keyboard navigation has to walk to it first.
    await page.keyboard.press('ArrowDown');
    await expect(codexItem).toBeFocused();
    await page.keyboard.press('Enter');
    // The "New codex" entry opens a Modal panel whose body wraps a
    // DirectoryBrowser. Both wrap their content in role="dialog" — the
    // Modal panel is the outer one (aria-label = title) and the nested
    // browser tags itself "Choose a directory". We anchor on the outer
    // by its accessible name so the scan target is unambiguous.
    const dialog = page.getByRole('dialog', { name: /new codex/i });
    await expect(dialog).toBeVisible();
    // Scope the scan to the modal panel — its content (SchemaForm or
    // DirectoryBrowser) is what we care about here, not the dimmed page
    // underneath (which we already scanned).
    const { violations } = await axe(page).include('.modal-panel').analyze();
    expect(violations, formatViolations(violations)).toEqual([]);
  });

  // ---- Dark-mode parity scans ------------------------------------------
  //
  // The scans above run against the default (light) theme. Re-run the 4
  // representative pages (the ones with the most distinct surfaces in
  // the original color-contrast investigation) with `data-theme="dark"`
  // applied so we catch any dark-only contrast regression. Limited to 4
  // tests intentionally — full per-state coverage would double the
  // suite's runtime for marginal additional signal.

  test('Today page · dark mode · no violations', async ({ page }) => {
    await page.goto('/?trace=1');
    await waitForBootstrap(page);
    await enableDarkTheme(page);
    const { violations } = await axe(page).analyze();
    expect(violations, formatViolations(violations)).toEqual([]);
  });

  test('Cove page · dark mode · no violations', async ({ page }) => {
    const { coveId } = await ids(page);
    await page.goto(`/calm/cove/${coveId}?trace=1`);
    await waitForBootstrap(page);
    await expect(page.getByRole('heading', { name: /scratch/i })).toBeVisible();
    await enableDarkTheme(page);
    const { violations } = await axe(page).analyze();
    expect(violations, formatViolations(violations)).toEqual([]);
  });

  test('Wave page · dark mode · no violations', async ({ page }) => {
    const { waveId } = await ids(page);
    await page.goto(`/calm/wave/${waveId}?trace=1`);
    await waitForBootstrap(page);
    await expect(page.getByRole('button', { name: /\+\s*add/i })).toBeVisible();
    await enableDarkTheme(page);
    const { violations } = await axe(page).analyze();
    expect(violations, formatViolations(violations)).toEqual([]);
  });

  test('Settings page · dark mode · no violations', async ({ page }) => {
    await page.goto('/calm/settings?trace=1');
    await expect(page.getByRole('textbox', { name: /http proxy/i })).toBeVisible({
      timeout: 15_000,
    });
    await enableDarkTheme(page);
    const { violations } = await axe(page).analyze();
    expect(violations, formatViolations(violations)).toEqual([]);
  });

  test('AddPanel open · dark mode · no violations on menu', async ({ page }) => {
    const { waveId } = await ids(page);
    await page.goto(`/calm/wave/${waveId}?trace=1`);
    await waitForBootstrap(page);
    await enableDarkTheme(page);
    const trigger = page.getByRole('button', { name: /\+\s*add/i });
    await expect(trigger).toBeVisible();
    await trigger.focus();
    await page.keyboard.press('Enter');
    await expect(page.getByRole('menu')).toBeVisible();
    const { violations } = await axe(page).include('[role="menu"]').analyze();
    expect(violations, formatViolations(violations)).toEqual([]);
  });

  test('Modal open · dark mode · no violations on dialog', async ({ page }) => {
    const { waveId } = await ids(page);
    await page.goto(`/calm/wave/${waveId}?trace=1`);
    await waitForBootstrap(page);
    await enableDarkTheme(page);
    const trigger = page.getByRole('button', { name: /\+\s*add/i });
    await trigger.focus();
    await page.keyboard.press('Enter');
    const codexItem = page.getByRole('menuitem', { name: /codex/i });
    const hasCodex = (await codexItem.count()) > 0;
    test.skip(!hasCodex, 'codex card kind not registered in this fixture');
    await page.keyboard.press('ArrowDown');
    await expect(codexItem).toBeFocused();
    await page.keyboard.press('Enter');
    const dialog = page.getByRole('dialog', { name: /new codex/i });
    await expect(dialog).toBeVisible();
    const { violations } = await axe(page).include('.modal-panel').analyze();
    expect(violations, formatViolations(violations)).toEqual([]);
  });
});
