// Axe-core scans for each major page + a couple of "open transient state"
// snapshots (modal up, AddPanel menu up). The point of these is to catch
// the bucket of things role/name keyboard tests can't see — color
// contrast, label/control association, landmark structure, etc.
//
// Coverage matrix:
//   Page                              | Describe block                  |
//   -----------------------------------|---------------------------------|
//   /calm/ (Today)                    | "Today page"                    |
//   /calm/area/<id>                   | "Area page"                     |
//   /calm/track/<id>                   | "Track page"                     |
//   /calm/settings                    | "Settings page"                 |
//   Track + AddPanel menu open         | "AddPanel open"                 |
//   Track + list view toggled on       | "Track list view"                |
//   Track + Modal open                 | "Modal open"                    |
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
// defer. The "common" pages (Today, Area, Track, Settings) MUST come out
// clean — if axe ever turns up violations on those, fix the source, don't
// silence the spec.

import { test, expect, type Page } from '@playwright/test';
import { AxeBuilder } from '@axe-core/playwright';
import {
  createIframeCard,
  resetReplayServer,
  createTrackInArea,
  seedTrackReport,
  seedTrackViewMode,
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
// had minted the default area and the areas query had refetched.
// Post-#175 the system area is hidden from the sidebar, so we anchor
// on the Today nav button instead: it's rendered as soon as the
// Sidebar mounts and is independent of whether useTodayTerminal's
// full bootstrap (system area → Today track → terminal card) completes.
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
//   - nested-interactive (PR #127): fixed by the TrackRow refactor — the
//     row is now a real `<button>` with a sibling delete `<button>`
//     inside a `.track-row-wrapper`.
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
//   - Excluded globally (not per-test) because every track with a spec
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

// Mint a fresh user area + track for the axe scans to operate on. After
// issue #175 the kernel's default Today terminal lives in a hidden
// system area that the sidebar can't reach, so we always create our
// own user area for these tests. We click sidebar / track-row
// affordances directly here (not keyboard-only) because this helper is
// just plumbing for the axe scans; the keyboard-only contract lives in
// `a11y-keyboard.spec.ts`.
async function ids(page: Page): Promise<{ areaId: string; trackId: string }> {
  await page.goto('/?trace=1');
  await waitForBootstrap(page);
  // Mint a user area via the sidebar "+ New area" affordance.
  const sidebarAreas = page.getByRole('navigation', { name: 'Areas' });
  const areaName = `axe area ${Date.now()}`;
  await sidebarAreas.getByRole('button', { name: /new area/i }).click();
  const nameInput = sidebarAreas.getByPlaceholder(/name/i);
  await expect(nameInput).toBeVisible();
  await nameInput.fill(areaName);
  await nameInput.press('Enter');
  // `exact: true` excludes the per-row "Delete area \"<name>\"" button
  // whose accessible name also contains areaName — strict mode otherwise
  // resolves to two buttons.
  const areaBtn = sidebarAreas.getByRole('button', { name: areaName, exact: true });
  await expect(areaBtn).toBeVisible();
  await areaBtn.click();
  await expect(page).toHaveURL(/\/calm\/area\/[^/]+(\?|$)/);
  const areaId = new URL(page.url()).pathname.split('/').pop()!;
  // Create a track via the API helper. PR 3's NewTaskForm now drives
  // the area-page "+ New track" CTA, but for axe scans (rendered-page
  // contracts) the track-create path is just plumbing — the REST-direct
  // helper keeps the scan setup cheap and decoupled from form UI
  // changes.
  const trackTitle = `axe track ${Date.now()}`;
  const track = await createTrackInArea(page.request, areaId, trackTitle);
  // #1147 S3 — the old `{ attachFolder: false }` argument existed only
  // to stop this second track from re-claiming the first one's invented
  // cwd (`area_folders.UNIQUE(path)`). `createTrackInArea` now sends no
  // cwd at all, so there is no claim to collide with and no knob.
  const source = await createTrackInArea(
    page.request,
    areaId,
    `axe backlink source ${Date.now()}`,
  );
  await seedTrackReport(
    page.request,
    source.id,
    'a11y backlink fixture',
    `[Cited report](neige://wave/${track.id})`,
  );
  await page.goto(`/calm/track/${track.id}`);
  await expect(page).toHaveURL(/\/calm\/track\/[^/]+(\?|$)/);
  return { areaId, trackId: track.id };
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

  test.describe('Area page', () => {
    for (const theme of THEMES) {
      test(`${theme} mode · no violations`, async ({ page }) => {
        const { areaId } = await ids(page);
        await page.goto(`/calm/area/${areaId}?trace=1`);
        await waitForBootstrap(page);
        // AreaPage paints its header (h1, eyebrow, …) synchronously once
        // areasQuery resolves. Wait for the H1 to appear before scanning
        // so we don't catch a half-rendered skeleton. We anchor on the
        // role rather than the area name (which now varies per run since
        // we mint our own in `ids()`).
        await expect(page.getByRole('heading', { level: 1 })).toBeVisible();
        await applyTheme(page, theme);
        const { violations } = await axe(page).analyze();
        expect(violations, formatViolations(violations)).toEqual([]);
      });
    }
  });

  test.describe('Track page', () => {
    for (const theme of THEMES) {
      test(`${theme} mode · no violations`, async ({ page }) => {
        const { trackId } = await ids(page);
        await page.goto(`/calm/track/${trackId}?trace=1`);
        await waitForBootstrap(page);
        // TrackGrid is lazy-loaded — wait for AddPanel to render before
        // scanning so the track page's full role tree is in the DOM.
        // The trigger is glyph-only since #594; aria-label "Add card".
        await expect(page.getByRole('button', { name: /add card/i })).toBeVisible();
        const backlinks = page.getByRole('region', { name: 'Backlinks' });
        await expect(
          backlinks.getByRole('link', { name: 'Cited report' }),
        ).toBeVisible();
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
        const { trackId } = await ids(page);
        await page.goto(`/calm/track/${trackId}?trace=1`);
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

  // Slice 9: the list-view alternative to TrackGrid. Same role/name
  // hygiene applies — labels, roles, landmark structure should come
  // out clean. The #594 demo removed the Grid↔List UI entry (the only
  // header view control is now the binary Grid↔Report switch), so we
  // enter list mode by seeding the per-track `view-mode` overlay via
  // REST — the same row the removed control wrote — before the page
  // loads, then scan the populated list state.
  test.describe('Track list view', () => {
    for (const theme of THEMES) {
      test(`${theme} mode · no violations`, async ({ page }) => {
        const { trackId } = await ids(page);
        await seedTrackViewMode(page.request, trackId, 'list');
        await createIframeCard(
          page.request,
          trackId,
          'https://example.invalid/axe-list-card',
          1,
        );
        await page.goto(`/calm/track/${trackId}?trace=1`);
        await waitForBootstrap(page);
        // Wait for the track page to fully render — the AddPanel trigger
        // (glyph-only since #594; aria-label "Add card") mounts with
        // the header and stays visible in list mode.
        const addBtn = page.getByRole('button', { name: /add card/i });
        await expect(addBtn).toBeVisible();
        // Post-#175 the track from `ids()` is freshly minted with zero
        // cards (the default Today PTY lives in the hidden system area,
        // not user-created tracks). Without at least one worker card the
        // list-view `<ul>` collapses to 0 height and Playwright reports
        // it as hidden. Seed an iframe worker card via REST so this scan
        // covers the populated list state without depending on PTY startup.
        // List mode lazily mounts; wait for the <ul> before the scan.
        await expect(page.getByRole('list', { name: /track cards/i })).toBeVisible({
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
        const { trackId } = await ids(page);
        await page.goto(`/calm/track/${trackId}?trace=1`);
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
