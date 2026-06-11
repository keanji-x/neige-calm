// Axe-core scans for each major page + a couple of "open transient state"
// snapshots (modal up, AddPanel menu up). The point of these is to catch
// the bucket of things role/name keyboard tests can't see — color
// contrast, label/control association, landmark structure, etc.
//
// Coverage matrix:
//   Page                              | Describe block                  |
//   -----------------------------------|---------------------------------|
//   /calm/ (Today)                    | "Today page"                    |
//   /calm/cove/<id>                   | "Cove page"                     |
//   /calm/wave/<id>                   | "Wave page"                     |
//   /calm/settings                    | "Settings page"                 |
//   Wave + AddPanel menu open         | "AddPanel open"                 |
//   Wave + list view toggled on       | "Wave list view"                |
//   Wave + Modal open                 | "Modal open"                    |
//
// Every describe block is parameterised over THEMES (light + dark) so the
// suite scans each route/state once per theme — catching dark-only
// contrast regressions that #133/#135 surfaced manually. Slice 3 of #142
// promoted this from "AddPanel + Modal only" to full parity.
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
import {
  createIframeCard,
  resetReplayServer,
  createWaveInCove,
  seedWaveViewMode,
} from './helpers/reset';

/** Themes the axe matrix scans every route under. `light` is the default
 *  the app boots into; `dark` is applied via `enableDarkTheme()` below
 *  after the page has rendered, before the scan runs. Keeping this as a
 *  single source of truth means a future "high-contrast" or "auto"
 *  variant can be added in one place. */
const THEMES = ['light', 'dark'] as const;
type Theme = (typeof THEMES)[number];

// Wait for the app shell to be ready. Pre-#175 this anchored on the
// Sidebar "Scratch" button — a stable signal that `useTodayTerminal`
// had minted the default cove and the coves query had refetched.
// Post-#175 the system cove is hidden from the sidebar, so we anchor
// on the Today nav button instead: it's rendered as soon as the
// Sidebar mounts and is independent of whether useTodayTerminal's
// full bootstrap (system cove → Today wave → terminal card) completes.
// In the replay-binary harness the terminal-card POST may surface a
// renderer-start error in CI and never set
// `localStorage['calm.todayCardId']`, so we can't anchor on that —
// the Today nav button is the equivalent "app shell is mounted"
// signal that works in both replay and live-daemon environments.
async function waitForBootstrap(page: Page): Promise<void> {
  await expect(
    page
      .getByRole('navigation', { name: 'Sidebar navigation' })
      .getByRole('button', { name: /^today$/i }),
  ).toBeVisible({ timeout: 15_000 });
}

// Flip the app into dark mode for the parallel dark-theme scans. Theme
// is owned by ThemeProvider (web/src/app/theme.tsx), which mirrors
// `resolved` into `document.documentElement.dataset.theme`. We can't
// invoke its setter from outside the bundle, but the dataset attribute
// is exactly what the `[data-theme="dark"]` selectors in `calm.css`
// consume — so writing it directly is sufficient to re-paint the
// cascade for axe's color-contrast probe. We wait on a `waitForFunction`
// checking the attribute *and* the computed background color of <body>
// (which should darken once the cascade re-evaluates) so the scan never
// races a half-applied theme. The ThemeProvider effect only re-fires
// when its React state changes, so our direct attribute write isn't
// clobbered by the bundle during the scan window.
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

/** Apply `theme` if it's not the default light mode. Centralised so each
 *  per-route test body only has to call `applyTheme(page, theme)` after
 *  its setup is complete — no branching in the call sites. */
async function applyTheme(page: Page, theme: Theme): Promise<void> {
  if (theme === 'dark') await enableDarkTheme(page);
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
//
// xterm subtrees are excluded globally from every scan. Rationale:
//   - xterm.js renders terminal output with its own ANSI/TTY palette
//     (e.g. `.xterm-fg-10` bold green), which is tied to terminal-user
//     expectations, not the app's design tokens — so the app's WCAG
//     contrast contract simply doesn't apply to that surface.
//   - The xterm container is presentational decoration: the real
//     interactive surface is the `.xterm-helper-textarea` (now
//     `tabindex=-1` per commit b9b6475), which AT users engage by
//     clicking into the terminal view. Surfacing the rendered glyphs
//     as inaccessible "text" is a category error.
//   - The previous attempt at hiding the xterm output (commit 20669b3:
//     `aria-hidden="true"` + `role="presentation"` on `.xterm-container`)
//     didn't satisfy axe-core's color-contrast walker — axe still
//     traversed into the subtree and flagged `:root`. `.exclude(...)` is
//     the documented escape hatch and applies before rule evaluation.
//   - Excluded globally (not per-test) because every wave with a spec
//     card or worker card mounts an xterm; gating one test at a time
//     would invariably let the same violation regress in a future test.
function axe(page: Page): AxeBuilder {
  return new AxeBuilder({ page })
    .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa', 'best-practice'])
    .disableRules(DEFERRED_RULES)
    .exclude('.xterm-container')
    .exclude('.xterm');
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

// Mint a fresh user cove + wave for the axe scans to operate on. After
// issue #175 the kernel's default Today terminal lives in a hidden
// system cove that the sidebar can't reach, so we always create our
// own user cove for these tests. We click sidebar / wave-row
// affordances directly here (not keyboard-only) because this helper is
// just plumbing for the axe scans; the keyboard-only contract lives in
// `a11y-keyboard.spec.ts`.
async function ids(page: Page): Promise<{ coveId: string; waveId: string }> {
  await page.goto('/?trace=1');
  await waitForBootstrap(page);
  // Mint a user cove via the sidebar "+ New cove" affordance.
  const sidebarCoves = page.getByRole('navigation', { name: 'Coves' });
  const coveName = `axe cove ${Date.now()}`;
  await sidebarCoves.getByRole('button', { name: /new cove/i }).click();
  const nameInput = sidebarCoves.getByPlaceholder(/name/i);
  await expect(nameInput).toBeVisible();
  await nameInput.fill(coveName);
  await nameInput.press('Enter');
  // `exact: true` excludes the per-row "Delete cove \"<name>\"" button
  // whose accessible name also contains coveName — strict mode otherwise
  // resolves to two buttons.
  const coveBtn = sidebarCoves.getByRole('button', { name: coveName, exact: true });
  await expect(coveBtn).toBeVisible();
  await coveBtn.click();
  await expect(page).toHaveURL(/\/calm\/cove\/[^/]+(\?|$)/);
  const coveId = new URL(page.url()).pathname.split('/').pop()!;
  // Create a wave via the API helper. PR 3's NewTaskForm now drives
  // the cove-page "+ New wave" CTA, but for axe scans (rendered-page
  // contracts) the wave-create path is just plumbing — the REST-direct
  // helper keeps the scan setup cheap and decoupled from form UI
  // changes.
  const waveTitle = `axe wave ${Date.now()}`;
  const wave = await createWaveInCove(page.request, coveId, waveTitle);
  await page.goto(`/calm/wave/${wave.id}`);
  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+(\?|$)/);
  return { coveId, waveId: wave.id };
}

test.describe('a11y · axe', () => {
  test.beforeEach(async ({ request }) => {
    // Hermetic per-test state — see `helpers/reset.ts`. Axe scans don't
    // mutate state themselves, but some tests click through the AddPanel
    // trigger / codex modals and we don't want their residue (extra
    // cards, opened modals' overlay payloads) leaking into the next
    // spec's DOM.
    await resetReplayServer(request);
  });

  // Each describe block below scans the same route/state twice — once
  // per theme — using identical assertions. The light pass is the
  // historical baseline; the dark pass guards the parallel cascade
  // (`[data-theme="dark"]` selectors in `calm.css`) against contrast /
  // landmark / labelling regressions that wouldn't show up at light.

  test.describe('Today page', () => {
    for (const theme of THEMES) {
      test(`${theme} mode · no violations`, async ({ page }) => {
        await page.goto('/?trace=1');
        await waitForBootstrap(page);
        await applyTheme(page, theme);
        const { violations } = await axe(page).analyze();
        expect(violations, formatViolations(violations)).toEqual([]);
      });
    }
  });

  test.describe('Cove page', () => {
    for (const theme of THEMES) {
      test(`${theme} mode · no violations`, async ({ page }) => {
        const { coveId } = await ids(page);
        await page.goto(`/calm/cove/${coveId}?trace=1`);
        await waitForBootstrap(page);
        // CovePage paints its header (h1, eyebrow, …) synchronously once
        // covesQuery resolves. Wait for the H1 to appear before scanning
        // so we don't catch a half-rendered skeleton. We anchor on the
        // role rather than the cove name (which now varies per run since
        // we mint our own in `ids()`).
        await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
        await applyTheme(page, theme);
        const { violations } = await axe(page).analyze();
        expect(violations, formatViolations(violations)).toEqual([]);
      });
    }
  });

  test.describe('Wave page', () => {
    for (const theme of THEMES) {
      test(`${theme} mode · no violations`, async ({ page }) => {
        const { waveId } = await ids(page);
        await page.goto(`/calm/wave/${waveId}?trace=1`);
        await waitForBootstrap(page);
        // WaveGrid is lazy-loaded — wait for AddPanel to render before
        // scanning so the wave page's full role tree is in the DOM.
        // The trigger is glyph-only since #594; aria-label "Add card".
        await expect(page.getByRole('button', { name: /add card/i })).toBeVisible();
        await applyTheme(page, theme);
        const { violations } = await axe(page).analyze();
        expect(violations, formatViolations(violations)).toEqual([]);
      });
    }
  });

  test.describe('Settings page', () => {
    for (const theme of THEMES) {
      test(`${theme} mode · no violations`, async ({ page }) => {
        await page.goto('/calm/settings?trace=1');
        // The form mounts with empty/default values; we still wait for
        // the first input to be present so the scan covers the real
        // DOM, not a pre-hydration shell.
        await expect(page.getByRole('textbox', { name: /http proxy/i })).toBeVisible({
          timeout: 15_000,
        });
        await applyTheme(page, theme);
        const { violations } = await axe(page).analyze();
        expect(violations, formatViolations(violations)).toEqual([]);
      });
    }
  });

  test.describe('AddPanel open', () => {
    for (const theme of THEMES) {
      test(`${theme} mode · no violations on menu`, async ({ page }) => {
        const { waveId } = await ids(page);
        await page.goto(`/calm/wave/${waveId}?trace=1`);
        await waitForBootstrap(page);
        await applyTheme(page, theme);
        // Open the menu via keyboard so we're scanning the same
        // transient state a real user would land in. Slice 7 may rework
        // the menu's keyboard semantics but the open-on-Enter contract
        // holds today. The trigger is glyph-only since #594; accessible
        // name is the aria-label "Add card" while closed.
        const trigger = page.getByRole('button', { name: /add card/i });
        await expect(trigger).toBeVisible();
        await trigger.focus();
        await page.keyboard.press('Enter');
        await expect(page.getByRole('menu')).toBeVisible();
        // Scope the scan to the menu region — scanning the whole
        // document would re-flag everything from the page-level scan
        // above. We explicitly want "is the menu itself ARIA-clean?".
        const { violations } = await axe(page).include('[role="menu"]').analyze();
        expect(violations, formatViolations(violations)).toEqual([]);
      });
    }
  });

  // Slice 9: the list-view alternative to WaveGrid. Same role/name
  // hygiene applies — labels, roles, landmark structure should come
  // out clean. The #594 demo removed the Grid↔List UI entry (the only
  // header view control is now the binary Grid↔Report switch), so we
  // enter list mode by seeding the per-wave `view-mode` overlay via
  // REST — the same row the removed control wrote — before the page
  // loads, then scan the populated list state.
  test.describe('Wave list view', () => {
    for (const theme of THEMES) {
      test(`${theme} mode · no violations`, async ({ page }) => {
        const { waveId } = await ids(page);
        await seedWaveViewMode(page.request, waveId, 'list');
        await createIframeCard(
          page.request,
          waveId,
          'https://example.invalid/axe-list-card',
          1,
        );
        await page.goto(`/calm/wave/${waveId}?trace=1`);
        await waitForBootstrap(page);
        // Wait for the wave page to fully render — the AddPanel trigger
        // (glyph-only since #594; aria-label "Add card") mounts with
        // the header and stays visible in list mode.
        const addBtn = page.getByRole('button', { name: /add card/i });
        await expect(addBtn).toBeVisible();
        // Post-#175 the wave from `ids()` is freshly minted with zero
        // cards (the default Today PTY lives in the hidden system cove,
        // not user-created waves). Without at least one worker card the
        // list-view `<ul>` collapses to 0 height and Playwright reports
        // it as hidden. Seed an iframe worker card via REST so this scan
        // covers the populated list state without depending on PTY startup.
        // List mode lazily mounts; wait for the <ul> before the scan.
        await expect(page.getByRole('list', { name: /wave cards/i })).toBeVisible({
          timeout: 5_000,
        });
        await applyTheme(page, theme);
        const { violations } = await axe(page).analyze();
        expect(violations, formatViolations(violations)).toEqual([]);
      });
    }
  });

  test.describe('Modal open', () => {
    for (const theme of THEMES) {
      test(`${theme} mode · no violations on dialog`, async ({ page }) => {
        const { waveId } = await ids(page);
        await page.goto(`/calm/wave/${waveId}?trace=1`);
        await waitForBootstrap(page);
        await applyTheme(page, theme);
        // Same path as the keyboard spec: open AddPanel (glyph-only
        // trigger since #594; aria-label "Add card" while closed), pick
        // the codex menuitem (the only built-in with a createSchema →
        // modal).
        const trigger = page.getByRole('button', { name: /add card/i });
        await trigger.focus();
        await page.keyboard.press('Enter');
        const codexItem = page.getByRole('menuitem', { name: /codex/i });
        const hasCodex = (await codexItem.count()) > 0;
        test.skip(!hasCodex, 'codex card kind not registered in this fixture');
        // Slice 7's roving-tabindex menu: ArrowDown from the (focused)
        // first menuitem to land on codex, then Enter activates *that*
        // item. `codexItem.press('Enter')` would fire keydown on the
        // codex button but the hook reads its internal `activeIndex` to
        // decide which item to activate — keyboard navigation has to
        // walk to it first.
        await page.keyboard.press('ArrowDown');
        await expect(codexItem).toBeFocused();
        await page.keyboard.press('Enter');
        // The "codex" menu entry opens a Modal panel (dialog title
        // "New codex") whose body wraps a
        // DirectoryBrowser. Both wrap their content in role="dialog" —
        // the Modal panel is the outer one (aria-label = title) and the
        // nested browser tags itself "Choose a directory". We anchor on
        // the outer by its accessible name so the scan target is
        // unambiguous.
        const dialog = page.getByRole('dialog', { name: /new codex/i });
        await expect(dialog).toBeVisible();
        // Scope the scan to the modal panel — its content (SchemaForm or
        // DirectoryBrowser) is what we care about here, not the dimmed
        // page underneath (which we already scanned).
        const { violations } = await axe(page).include('.modal-panel').analyze();
        expect(violations, formatViolations(violations)).toEqual([]);
      });
    }
  });
});
