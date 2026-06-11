// Keyboard-driven end-to-end coverage for issue #56 — the a11y contract.
//
// Once on the test surface, navigation is keyboard-only: Tab, Shift+Tab,
// Enter, Space, Escape, F2, Arrow keys. The point is to prove an AI agent
// (or any keyboard-only user, screen-reader user, etc.) can drive every
// flow the product cares about using role/name and key events alone.
//
// The one carve-out is entry-point setup (sidebar → cove → wave) in the
// list-view reorder test: that test clicks its way to the wave surface to
// sidestep `tabUntil` brittleness across test runs that accumulate waves.
// Sidebar / cove navigation has its own dedicated keyboard coverage
// elsewhere in this suite — the plumbing clicks there are not the
// contract under test. Every such click carries an inline comment.
//
// No `.fill()` — all text input goes through `keyboard.type()` so the
// input handlers fire the same way they would for a real keyboard user.
//
// The suite runs under the Playwright `a11y` project — see
// `playwright.config.ts`. That project starts a Vite dev server in front
// of `cargo run --bin replay --serve`, which boots an in-memory kernel
// with a fixture preloaded. The replay binary seeds the events table but
// does NOT project them onto the entity tables (see
// `crates/calm-server/src/replay.rs`); that's intentional, so the entity
// tables start empty. Issue #175 — the web app's Today page then
// auto-creates a hidden **system** cove + "Today" wave + terminal card
// via `useTodayTerminal` on first paint; that cove is filtered out of
// `GET /api/coves` by default, so the sidebar never renders it. Each
// `beforeEach` below mints an `Atlas` **user** cove via the REST API
// so the keyboard tests have a stable sidebar anchor to navigate from.
//
// Where it matters, we pair the UI assertion with an event-trace
// assertion (`getEventTrace` / `waitForEvent` from `helpers/trace.ts`) so
// the test proves both halves of the role/name + event contract from #56.

import { test, expect, type Page } from '@playwright/test';
import {
  createIframeCard,
  createUserCove,
  createWaveInCove,
  resetReplayServer,
  seedWaveViewMode,
} from './helpers/reset';
import { clearEventTrace, getEventTrace, waitForEvent } from './helpers/trace';

interface FocusInfo {
  tag: string;
  role: string | null;
  name: string | null;
  description: string | null;
  /** `className` of the focused element, lowercased. Used to disambiguate
   *  same-named role buttons when role + name alone aren't unique — e.g.
   *  two buttons named "Today" share the page (sidebar nav, WaveRow)
   *  and only the WaveRow carries `wave-row` in its class list. */
  className: string;
}

// Tab forward until `predicate(activeElement)` matches, then return.
// Bounded at `maxSteps` to fail fast on a stuck cycle rather than hanging
// the whole run on the Playwright default timeout. We use this whenever
// the spec needs to land on a specific control without depending on the
// exact tab-stop count — that count changes when components are added or
// reordered, but the focused element's role/name doesn't.
//
// We snapshot the focused element's tag, role, aria-label, title,
// textContent, and className — the predicate gets the union so callers
// can match on whichever is most stable. The `className` field
// disambiguates same-role/same-name buttons when role + name alone
// aren't unique (e.g. two "Today" buttons share the page — sidebar
// nav, WaveRow; only the WaveRow has `wave-row` in its classes).
async function tabUntil(
  page: Page,
  predicate: (info: FocusInfo) => boolean,
  maxSteps = 80,
): Promise<void> {
  // First step is always a Tab so we move OFF of document.body — its
  // textContent contains every label on the page, which would falsely
  // satisfy almost any name-substring predicate.
  await page.keyboard.press('Tab');
  for (let i = 0; i < maxSteps; i++) {
    const info = await page.evaluate(() => {
      const el = document.activeElement as HTMLElement | null;
      if (!el || el === document.body) {
        return { tag: '', role: null, name: null, description: null, className: '' };
      }
      // Accessible name resolution, simplified: aria-label > aria-
      // labelledby (resolved to text) > <label for=...> (for form
      // controls) > the host's own textContent. Good enough for our
      // predicates without pulling in the full ARIA-1.1 algorithm.
      const ariaLabel = el.getAttribute('aria-label');
      const labelledBy = el.getAttribute('aria-labelledby');
      let labelledByText: string | null = null;
      if (labelledBy) {
        const ids = labelledBy.split(/\s+/);
        const parts = ids
          .map((id) => document.getElementById(id)?.textContent?.trim() ?? '')
          .filter(Boolean);
        if (parts.length) labelledByText = parts.join(' ');
      }
      // For <input> / <select> / <textarea>, the accessible name often
      // comes from the wrapping <label>. We look it up explicitly so
      // form-field predicates (e.g. "http proxy") can match.
      let labelText: string | null = null;
      const isFormControl = ['INPUT', 'SELECT', 'TEXTAREA'].includes(el.tagName);
      if (isFormControl) {
        const id = el.id;
        if (id) {
          const lbl = document.querySelector('label[for="' + id + '"]');
          if (lbl) labelText = lbl.textContent?.trim() ?? null;
        }
        if (!labelText) {
          const wrappingLabel = el.closest('label');
          if (wrappingLabel) labelText = wrappingLabel.textContent?.trim() ?? null;
        }
      }
      // aria-describedby → joined text of referenced elements. Mirrors
      // the aria-labelledby path above; used by the Cove/Wave rename
      // surfaces to convey the rename verb without polluting the name.
      const describedBy = el.getAttribute('aria-describedby');
      let describedByText: string | null = null;
      if (describedBy) {
        const ids = describedBy.split(/\s+/);
        const parts = ids
          .map((id) => document.getElementById(id)?.textContent?.trim() ?? '')
          .filter(Boolean);
        if (parts.length) describedByText = parts.join(' ');
      }
      return {
        tag: el.tagName.toLowerCase(),
        role: el.getAttribute('role'),
        name:
          ariaLabel ??
          labelledByText ??
          labelText ??
          el.getAttribute('title') ??
          (el.textContent ? el.textContent.trim().slice(0, 80) : null),
        description: describedByText,
        className: (el.getAttribute('class') ?? '').toLowerCase(),
      };
    });
    if (predicate(info)) return;
    await page.keyboard.press('Tab');
  }
  throw new Error('tabUntil: predicate never matched within ' + maxSteps + ' steps');
}

// Wait for the auto-bootstrap to land. `useTodayTerminal` runs on first
// paint of the Today page and creates a hidden system cove + "Today"
// wave + terminal card (issue #175 — the system cove is not visible
// in the sidebar). The `beforeEach` below also mints an `Atlas` user
// cove via REST so the keyboard tests have a stable sidebar anchor;
// this helper waits for that user cove to render (the WS feed
// invalidates the coves query and the live render picks it up).
async function waitForBootstrap(page: Page): Promise<void> {
  // `exact: true` excludes the per-row "Delete cove \"Atlas\"" button
  // whose accessible name also contains "Atlas" — strict mode otherwise
  // resolves to two buttons.
  await expect(
    page.locator('aside.side').getByRole('button', { name: 'Atlas', exact: true }),
  ).toBeVisible({ timeout: 15_000 });
  // Also wait for the trace buffer to come into existence so subsequent
  // `clearEventTrace` / `waitForEvent` calls have a buffer to read.
  await page.waitForFunction(() => Array.isArray(window.__neigeEvents__));
}

function containsTerminalCard(value: unknown): boolean {
  if (Array.isArray(value)) {
    return value.some(containsTerminalCard);
  }
  if (!value || typeof value !== 'object') {
    return false;
  }
  const record = value as Record<string, unknown>;
  if (
    (record.kind === 'terminal' || record.type === 'terminal') &&
    (typeof record.id === 'string' || typeof record.card_id === 'string')
  ) {
    return true;
  }
  return Object.values(record).some(containsTerminalCard);
}

test.describe('a11y · keyboard-only navigation', () => {
  test.beforeEach(async ({ page, request }) => {
    // Reset the replay binary's in-memory repo + event log first, so the
    // page navigation below mounts against a hermetic starting state
    // regardless of what an earlier spec did. The endpoint is mounted
    // only by `replay --serve` (see `crates/calm-server/src/bin/replay.rs`).
    // Without this hook, accumulating cove/wave/card mutations across
    // tests cause flakes — see issue #56 followup.
    await resetReplayServer(request);
    // Block Google Fonts. `index.html` loads a `<link rel="stylesheet"
    // href="https://fonts.googleapis.com/...">` that, in restricted-
    // network test environments, hangs subsequent `page.goto` calls
    // because Chrome never fires `load` while the stylesheet request
    // is pending.
    await page.route('**://fonts.googleapis.com/**', (route) => route.abort());
    await page.route('**://fonts.gstatic.com/**', (route) => route.abort());
    // Issue #175 — the kernel hides the system cove that hosts the
    // default Today terminal from the sidebar. Mint a user-visible
    // `Atlas` cove + `Today` wave via the REST API so the keyboard
    // tests below have a stable sidebar anchor (they all
    // `tabUntil(... /atlas/i)`) and a Today wave under it (the
    // WaveRow tests anchor on /today/i inside the Waves region). The
    // replay server's `POST /api/coves` + `POST /api/waves` are the
    // same handlers production uses; the live frontend invalidates the
    // coves / waves queries on the resulting WS events and renders the
    // new rows without a reload.
    const atlas = await createUserCove(request, 'Atlas');
    await createWaveInCove(request, atlas.id, 'Today');
    // Every spec opens the app with the trace ring buffer enabled so that
    // event assertions can read `window.__neigeEvents__`. baseURL is set
    // by the `a11y` project, so we just append the param.
    await page.goto('/?trace=1');
    await waitForBootstrap(page);
    // Clear the trace once bootstrap is done so each test's assertions
    // see a clean ring buffer — the bootstrap path generates its own
    // events (`cove.updated`, `wave.updated`, …) that would otherwise
    // pollute per-test trace expectations.
    await clearEventTrace(page);
  });

  test('Today → Cove via keyboard', async ({ page }) => {
    // Tab forward from the document start until focus lands on the
    // Atlas cove nav button in the sidebar. Its textContent-derived
    // helper name includes the wave-count badge (e.g. "Atlas1"), so
    // match the nav button class plus the cove-name prefix.
    await tabUntil(
      page,
      (info) =>
        info.className.includes('cove-nav') &&
        (info.name?.toLowerCase().startsWith('atlas') ?? false),
    );
    // Activate the cove. The Sidebar's cove rows are real <button>s, so
    // Enter is the canonical activation key — Space would also work, but
    // Enter is what a screen reader announces ("Activate").
    await page.keyboard.press('Enter');
    // Router transitions to /calm/cove/<id> — the cove-id portion is
    // opaque (kernel-generated UUID), so we just match the prefix.
    await expect(page).toHaveURL(/\/calm\/cove\/[^/]+(\?|$)/);
    // The CovePage's <h1> title button renders the cove name + period
    // ("Atlas."). Asserting it is visible proves the route actually
    // mounted, not just that the URL changed.
    await expect(page.getByRole('heading', { name: /atlas/i })).toBeVisible();
  });

  // Issue #250 PR 3 — keyboard-driven wave creation through NewTaskForm.
  // The cove-page "+ New wave" CTA expands inline into the shared
  // configuration card (task description + cwd + cove inference); the
  // legacy single-line title input is gone (replaced by the full form
  // per the issue's "all creation entrypoints must go through the
  // same configuration card" comment).
  //
  // Keyboard contract exercised here:
  //   - Tab lands on the "+ New wave" CTA.
  //   - Enter expands it into the form; focus auto-lands on the task
  //     description textarea (NewTaskForm's useEffect → focus).
  //   - Tab from there reaches the cwd input.
  //   - Enter on the cwd input submits (the form binds Enter-to-submit
  //     specifically on cwd; the title textarea preserves Enter for
  //     newlines, per the form's design).
  //   - Successful submit navigates to /calm/wave/<id>.
  test('Cove → New wave via keyboard creates a wave', async ({ page }) => {
    // First land on the cove page via keyboard (same path as above).
    await tabUntil(
      page,
      (info) =>
        info.className.includes('cove-nav') &&
        (info.name?.toLowerCase().startsWith('atlas') ?? false),
    );
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/calm\/cove\/[^/]+(\?|$)/);

    // Tab to the "+ New wave" CTA. Its accessible name comes from the
    // button text (no aria-label); the title attribute is "New wave".
    await tabUntil(page, (info) => /new wave/i.test(info.name ?? ''));
    // Enter expands the CTA into the inline NewTaskForm.
    await page.keyboard.press('Enter');

    // The form auto-focuses its task description textarea on mount
    // (see NewTaskForm.tsx `useEffect → queueMicrotask(focus)`).
    const descriptionInput = page.getByLabel(/task description/i);
    await expect(descriptionInput).toBeFocused();

    // Type the description. Avoid Enter here — it would insert a
    // newline (the textarea is multi-line).
    const title = `a11y wave ${Date.now()}`;
    await page.keyboard.type(title);

    // Tab to the cwd input. The cove section between cwd and the
    // actions isn't a direct keyboard-focusable target on first
    // paint (we're still in the resolve-debounce window) so a single
    // Tab from the textarea lands the cwd input.
    await page.keyboard.press('Tab');
    const cwdInput = page.getByLabel(/working directory/i);
    await expect(cwdInput).toBeFocused();

    // Unique absolute cwd so concurrent runs / re-runs don't trip
    // the cove_folders UNIQUE(path) backstop.
    const cwd = `/tmp/playwright-a11y-${Date.now()}`;
    await page.keyboard.type(cwd);

    // Enter on the cwd input submits the form. The form's auto-
    // match-cove path picks the surrounding Atlas cove (CovePage
    // passes `defaultCoveId={cove.id}`); since no folder claim
    // covers `cwd`, the submit goes through with
    // `attach_folder: true`.
    await page.keyboard.press('Enter');

    // The CovePage's onWaveCreated handler navigates straight to the
    // new wave's detail page (router.tsx wires
    // `go({name:'wave',id:...})`).
    await expect(page).toHaveURL(/\/calm\/wave\/[^/]+(\?|$)/, { timeout: 10_000 });
    await expect(page.getByText(title, { exact: false }).first()).toBeVisible();

    // Event-trace contract: the create round-trip emits a wave.updated
    // for the new wave. We wait for it (poll-based) rather than reading
    // the trace once and racing the WS pump.
    await waitForEvent(page, 'wave.updated');
    const trace = await getEventTrace(page);
    expect(trace.map((e) => e.ev)).toContain('wave.updated');
  });

  test('Wave → AddPanel opens with Enter and closes with Escape', async ({ page }) => {
    // Navigate to a wave page via keyboard so the AddPanel trigger
    // exists in the DOM. We use the auto-created "Today" wave under the
    // Atlas cove — it's the only wave that exists at this point.
    await tabUntil(
      page,
      (info) =>
        info.className.includes('cove-nav') &&
        (info.name?.toLowerCase().startsWith('atlas') ?? false),
    );
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/calm\/cove\/[^/]+(\?|$)/);
    // From the cove page, the "Today" wave row is a real <button> with
    // the wave title as its accessible name (see WaveRow.tsx). Two
    // buttons share the name "Today" — sidebar nav and the WaveRow — so
    // we filter on `wave-row` in className to land on the row button
    // specifically.
    await tabUntil(
      page,
      (info) =>
        info.tag === 'button' &&
        info.className.split(/\s+/).includes('wave-row') &&
        /today/i.test(info.name ?? ''),
    );
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/calm\/wave\/[^/]+(\?|$)/);

    // Tab to the AddPanel trigger. It's a real <button> with
    // aria-haspopup="menu" + aria-expanded — glyph-only (+/×) since the
    // #594 demo, so its accessible name comes from aria-label
    // ("Add card" closed, "Close add menu" open).
    await tabUntil(page, (info) => /add card/i.test(info.name ?? ''));
    // Enter toggles the menu open. The popover renders a <ul role="menu">
    // immediately on open.
    await page.keyboard.press('Enter');
    const menu = page.getByRole('menu');
    await expect(menu).toBeVisible();

    // Escape closes the menu. AddPanel's keydown listener handles this
    // globally while open; we don't need focus to be inside the menu.
    await page.keyboard.press('Escape');
    await expect(menu).toBeHidden();
  });

  test('Wave → AddPanel terminal menuitem creates a card via keyboard', async ({
    page,
  }) => {
    // Navigate to the Atlas/Today wave via keyboard so the AddPanel
    // trigger is reached through the same user-visible path as the
    // broader keyboard contract tests.
    await tabUntil(
      page,
      (info) =>
        info.className.includes('cove-nav') &&
        (info.name?.toLowerCase().startsWith('atlas') ?? false),
    );
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/calm\/cove\/[^/]+(\?|$)/);
    await tabUntil(
      page,
      (info) =>
        info.tag === 'button' &&
        info.className.split(/\s+/).includes('wave-row') &&
        /today/i.test(info.name ?? ''),
    );
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/calm\/wave\/[^/]+(\?|$)/);
    const waveUrl = page.url();
    const waveId = waveUrl.match(/\/calm\/wave\/([^/?#]+)/)?.[1];
    expect(waveId, `wave id parsed from ${waveUrl}`).toBeTruthy();

    // Old #632-removed coverage: focus the Add-card button, open with
    // Enter, assert the terminal menuitem receives focus, then activate
    // it with Enter.
    await tabUntil(page, (info) => /add card/i.test(info.name ?? ''));
    await expect(page.getByRole('button', { name: /add card/i })).toBeFocused();
    await page.keyboard.press('Enter');
    const menu = page.getByRole('menu');
    await expect(menu).toBeVisible();
    const terminalItem = page.getByRole('menuitem', { name: /terminal/i }).first();
    await expect(terminalItem).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(menu).toBeHidden();

    // Assert the keyboard activation persisted a terminal card row.
    // Poll the replay API instead of sleeping: the card lands via the
    // same async mutation/WS path as production.
    await expect
      .poll(
        async () => {
          const response = await page.request.get(`/api/waves/${encodeURIComponent(waveId!)}`);
          if (!response.ok()) {
            return false;
          }
          return containsTerminalCard(await response.json());
        },
        { timeout: 10_000 },
      )
      .toBe(true);
  });

  // Slice 7 wires the full WAI-ARIA menu keyboard contract on AddPanel:
  // ArrowDown/Up cycle through menuitems (with wrap), Home/End jump to
  // first/last, single letters typeahead-jump to the first match, Escape
  // closes and returns focus to the trigger, and activation closes plus
  // returns focus to the trigger before the onSelect handler runs.
  test('AddPanel: arrow keys, Home/End, typeahead, focus restore', async ({
    page,
  }) => {
    // Navigate to a wave page via keyboard. We use the auto-created
    // "Today" wave under the Atlas cove — the only one that exists at
    // bootstrap time on the replay fixture.
    await tabUntil(
      page,
      (info) =>
        info.className.includes('cove-nav') &&
        (info.name?.toLowerCase().startsWith('atlas') ?? false),
    );
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/calm\/cove\/[^/]+(\?|$)/);
    // WaveRow is a real <button>; filter on `wave-row` className to
    // disambiguate from the sidebar Today nav button (both real
    // <button>s with the same accessible name).
    await tabUntil(
      page,
      (info) =>
        info.tag === 'button' &&
        info.className.split(/\s+/).includes('wave-row') &&
        /today/i.test(info.name ?? ''),
    );
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/calm\/wave\/[^/]+(\?|$)/);

    // Tab to the AddPanel trigger (glyph-only since #594; accessible
    // name "Add card" while closed) and capture it for the focus-restore
    // assertion at the end of the spec. The name-based locator resolves
    // at every point we assert on it — the menu is closed there, so the
    // aria-label has flipped back from "Close add menu" to "Add card".
    await tabUntil(page, (info) => /add card/i.test(info.name ?? ''));
    const trigger = page.getByRole('button', { name: /add card/i });
    await expect(trigger).toBeFocused();
    await page.keyboard.press('Enter');
    const menu = page.getByRole('menu');
    await expect(menu).toBeVisible();

    // On open the hook focuses the first menuitem. The order depends on
    // which builtins register, but the fixture today gives us at least
    // "terminal" + "codex" (registered in that order in
    // `cards/builtins/index.ts`). Each menuitem renders a card-head-style
    // letter-avatar (aria-hidden) before its uppercase label; the
    // accessible name is the lowercase kind word.
    const items = page.getByRole('menuitem');
    const itemCount = await items.count();
    expect(itemCount).toBeGreaterThanOrEqual(2);

    // Initial focus on the first item.
    await expect(items.nth(0)).toBeFocused();

    // ArrowDown moves to the second item.
    await page.keyboard.press('ArrowDown');
    await expect(items.nth(1)).toBeFocused();

    // ArrowUp moves back. From the first, ArrowUp wraps to the last.
    await page.keyboard.press('ArrowUp');
    await expect(items.nth(0)).toBeFocused();
    await page.keyboard.press('ArrowUp');
    await expect(items.nth(itemCount - 1)).toBeFocused();

    // End jumps to the last (already there — exercise the keybind anyway).
    await page.keyboard.press('Home');
    await expect(items.nth(0)).toBeFocused();
    await page.keyboard.press('End');
    await expect(items.nth(itemCount - 1)).toBeFocused();

    // Typeahead: from the first item, pressing a letter moves to the
    // next item in cyclic order whose accessible name starts with it.
    // Use the last item's first letter as a deterministic key, then
    // compute the exact expected target for the current registry.
    await page.keyboard.press('Home');
    await expect(items.nth(0)).toBeFocused();
    const texts = (await items.allTextContents()).map((text) => text.trim());
    const letter = texts[itemCount - 1][0]!.toLowerCase();
    let expected = 0;
    for (let k = 1; k <= itemCount; k++) {
      const idx = k % itemCount;
      if ((texts[idx][0] ?? '').toLowerCase() === letter) {
        expected = idx;
        break;
      }
    }
    await page.keyboard.press(letter);
    await expect(items.nth(expected)).toBeFocused();

    // Escape closes the menu and returns focus to the trigger.
    await page.keyboard.press('Escape');
    await expect(menu).toBeHidden();
    await expect(trigger).toBeFocused();

    // Re-open via the trigger, then activate the first menuitem with
    // Enter. The menu closes; focus restores to the trigger.
    await page.keyboard.press('Enter');
    await expect(menu).toBeVisible();
    await expect(items.nth(0)).toBeFocused();
    // First item is "terminal" (zero-config) — Enter creates a
    // card immediately and closes the menu. Other entries open a
    // SchemaForm; either way the AddPanel itself is gone.
    await page.keyboard.press('Enter');
    await expect(menu).toBeHidden();
    // Focus is restored to the trigger button regardless of whether
    // onSelect opened a modal (modal restore is a separate layer).
    // The terminal create path doesn't open a modal, so the trigger
    // should be focused directly.
    await expect(trigger).toBeFocused();
  });

  test('Modal: opens with Enter, traps Tab, Escape closes and restores focus', async ({
    page,
  }) => {
    // Navigate to the wave page via keyboard.
    await tabUntil(
      page,
      (info) =>
        info.className.includes('cove-nav') &&
        (info.name?.toLowerCase().startsWith('atlas') ?? false),
    );
    await page.keyboard.press('Enter');
    // WaveRow is a real <button>; filter on `wave-row` className to
    // disambiguate from the sidebar Today nav button (both real
    // <button>s with the same accessible name).
    await tabUntil(
      page,
      (info) =>
        info.tag === 'button' &&
        info.className.split(/\s+/).includes('wave-row') &&
        /today/i.test(info.name ?? ''),
    );
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/calm\/wave\/[^/]+(\?|$)/);

    // Open the AddPanel and pick a menuitem that opens a Modal. In the
    // current registry that's "codex" — terminal has no createSchema
    // so it creates immediately. If no codex entry is registered (e.g.
    // plugins not loaded in this fixture) we gracefully skip the modal-
    // focus assertions instead of red-flagging the suite.
    await tabUntil(page, (info) => /add card/i.test(info.name ?? ''));
    // Capture the trigger button so we can verify focus restore later.
    // Name-based locator: "Add card" is the trigger's aria-label while
    // the menu is closed (it flips to "Close add menu" while open, but
    // no assertion below resolves it in that state).
    const trigger = page.getByRole('button', { name: /add card/i });
    await page.keyboard.press('Enter');
    const menu = page.getByRole('menu');
    await expect(menu).toBeVisible();

    const codexItem = page.getByRole('menuitem', { name: /codex/i });
    const hasCodex = (await codexItem.count()) > 0;
    test.skip(!hasCodex, 'codex card kind not registered in this fixture');

    // Slice 7 wires real menu-keyboard semantics. The menu's first item
    // ("terminal") gets initial focus on open; we navigate down to
    // "codex" via ArrowDown (its registry position) before pressing
    // Enter. We could also type 'c' (typeahead) — both paths satisfy the
    // contract; ArrowDown is the more universal choice since it doesn't
    // depend on label spelling.
    await page.keyboard.press('ArrowDown');
    await expect(codexItem).toBeFocused();
    await page.keyboard.press('Enter');

    // Modal mounts with role="dialog" + aria-modal="true"; the panel is
    // focused initially (or its first focusable child, depending on what
    // SchemaForm/DirectoryBrowser put first). Two dialogs may render —
    // the outer Modal panel and the nested DirectoryBrowser — so anchor
    // on the outer by accessible name.
    const dialog = page.getByRole('dialog', { name: /new codex/i });
    await expect(dialog).toBeVisible();

    // Tab once and re-tab — focus must remain inside the dialog (the
    // focus trap from Slice 2). We check by reading the active element
    // and asserting `dialog.contains(...)` after each press.
    await page.keyboard.press('Tab');
    await expect
      .poll(async () =>
        page.evaluate(() => {
          // Use the outer modal panel as the trap boundary — the nested
          // DirectoryBrowser also carries role="dialog" but lives *inside*
          // the modal panel, so containment via `.modal-panel` correctly
          // counts focus on the browser as "still inside the trap".
          const dlg = document.querySelector('.modal-panel');
          const active = document.activeElement;
          return !!dlg && !!active && dlg.contains(active);
        }),
      )
      .toBe(true);
    await page.keyboard.press('Tab');
    await page.keyboard.press('Tab');
    expect(
      await page.evaluate(() => {
        const dlg = document.querySelector('.modal-panel');
        const active = document.activeElement;
        return !!dlg && !!active && dlg.contains(active);
      }),
    ).toBe(true);

    // Escape closes the modal. With Slice 7's menu-close focus restore
    // in place, the chain is now: AddPanel's `closeAndRestoreFocus` runs
    // BEFORE the modal opens (it fires synchronously inside `activate`
    // when Enter activates the menuitem). So by the time Modal captures
    // `previouslyFocusedRef`, the trigger button is already the active
    // element — and Modal's own restore on close returns focus straight
    // to the trigger. The combined effect: Escape on the modal → focus
    // back on the AddPanel trigger.
    await page.keyboard.press('Escape');
    await expect(dialog).toBeHidden();
    await expect(trigger).toBeFocused();
  });

  test('Wave title rename: F2 enters, Enter commits, focus restored', async ({
    page,
  }) => {
    // Land on the wave page via keyboard.
    await tabUntil(
      page,
      (info) =>
        info.className.includes('cove-nav') &&
        (info.name?.toLowerCase().startsWith('atlas') ?? false),
    );
    await page.keyboard.press('Enter');
    // WaveRow is a real <button>; filter on `wave-row` className to
    // disambiguate from the sidebar Today nav button (both real
    // <button>s with the same accessible name).
    await tabUntil(
      page,
      (info) =>
        info.tag === 'button' &&
        info.className.split(/\s+/).includes('wave-row') &&
        /today/i.test(info.name ?? ''),
    );
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/calm\/wave\/[^/]+(\?|$)/);

    // The wave title display is a <span role="button"> — see
    // pages/Wave.tsx. After #56 followup its accessible name is just the
    // wave title (e.g. "Today"); the rename verb is conveyed via
    // aria-describedby. We match the span by its description to land
    // specifically on the rename target (and not on the cove crumb
    // button, which also carries text but no rename description).
    await tabUntil(page, (info) => /^rename wave$/i.test(info.description ?? ''));
    // F2 is the documented rename shortcut (Windows convention). Enter
    // also works (Slice 3) but we exercise F2 here for variety.
    await page.keyboard.press('F2');

    // The display swaps to an <input aria-label="Wave title">. It auto-
    // focuses via the queueMicrotask in startRename().
    const input = page.getByLabel('Wave title');
    await expect(input).toBeFocused();

    // The input pre-selects the existing value (.select() in startRename);
    // typing replaces it.
    const newTitle = `renamed ${Date.now()}`;
    await page.keyboard.type(newTitle);
    await page.keyboard.press('Enter');

    // Commit collapses the input back to the display span and returns
    // focus there. After the WS event lands the display shows the new
    // title (we wait for both halves so the test is order-independent).
    await waitForEvent(page, 'wave.updated');
    // Disambiguate via `description` — the rename span carries
    // `aria-describedby` → "Rename wave" (see §5 of a11y-contract.md),
    // while the sibling Delete button matches `newTitle` only as a
    // substring inside its own accessible name ("Delete wave \"<title>\"")
    // and would otherwise collide here under Playwright strict mode.
    const display = page.getByRole('button', { name: newTitle, description: 'Rename wave' });
    await expect(display).toBeVisible();
    await expect(display).toBeFocused();
  });

  // Slice 9 — list-view alternative to the WaveGrid. The header now exposes
  // a single Grid / List / Report cycle button backed by the same
  // per-wave view-mode overlay row that this spec seeds via REST.
  // List view replaces the RGL grid with a semantic `<ul>` whose `<li>`
  // items use roving tabindex; Alt+ArrowUp / Alt+ArrowDown reorder the
  // focused card by swapping `card.sort` via the existing optimistic
  // mutation.
  test('Wave: toggle to list view, reorder with Alt+Arrow, persist across reload', async ({
    page,
  }) => {
    // Click (not keyboard) into the wave: skips tabUntil to avoid tab-count
    // brittleness when previous tests accumulate waves. The Atlas cove
    // and its auto-created Today wave are the stable entrypoints; this
    // test exercises the list-view toggle + Alt+Arrow reorder contract,
    // not the sidebar / cove navigation (those have their own keyboard
    // coverage elsewhere in this suite).
    // `exact: true` excludes the per-row "Delete cove \"Atlas\"" button
    // whose accessible name also contains "Atlas" (strict mode otherwise
    // resolves to two buttons).
    await page
      .locator('aside.side')
      .getByRole('button', { name: 'Atlas', exact: true })
      .click();
    await expect(page).toHaveURL(/\/calm\/cove\/[^/]+(\?|$)/);
    // Click into the auto-bootstrapped "Today" wave row. WaveRow is a
    // real <button> with the wave title as its accessible name (see
    // WaveRow.tsx). The CovePage wraps its single sorted wave list in a
    // `<section aria-label="Waves">` landmark so role-scoped queries
    // can disambiguate the row from the sidebar "Today" nav button
    // (both real <button>s with the same accessible name).
    // Click (not keyboard): same rationale as the cove-nav click above —
    // skip tabUntil to avoid tab-count brittleness across accumulating waves.
    await page
      .getByRole('region', { name: 'Waves' })
      .getByRole('button', { name: /today/i })
      .first()
      .click();
    await expect(page).toHaveURL(/\/calm\/wave\/[^/]+(\?|$)/);
    const waveUrl = page.url();

    // Add two renderer-free worker cards so the reorder test has two
    // list items to swap. Card creation itself is covered by the
    // AddPanel tests above; this spec's contract is the list surface +
    // Alt+Arrow reorder. Use direct iframe cards here so the replay
    // harness does not depend on a terminal/codex daemon being available.
    const waveId = waveUrl.match(/\/calm\/wave\/([^/?#]+)/)?.[1];
    expect(waveId, `wave id parsed from ${waveUrl}`).toBeTruthy();
    await createIframeCard(
      page.request,
      waveId!,
      'https://example.invalid/e2e-list-card-1',
      1,
    );
    await createIframeCard(
      page.request,
      waveId!,
      'https://example.invalid/e2e-list-card-2',
      2,
    );

    // Enter list mode by seeding the per-wave `view-mode` overlay via REST,
    // then reload. `?trace=1`
    // re-arms the event ring buffer for the reorder assertions below
    // (the flag is read once per page load, and the client-side route
    // URL we captured may not carry it).
    await seedWaveViewMode(page.request, waveId!, 'list');
    await page.goto(`/calm/wave/${waveId}?trace=1`);
    await page.waitForFunction(() => Array.isArray(window.__neigeEvents__));

    // List view: cards now render as semantic <li>. Wait for the list
    // to mount (it's lazy-loaded — same chunk pattern as WaveGrid).
    const list = page.getByRole('list', { name: /wave cards/i });
    await expect(list).toBeVisible({ timeout: 5_000 });
    const items = page.getByRole('listitem');
    // Poll until at least two items have *both* mounted AND have their
    // data-card-id stamped — the kernel ack for the new card has to
    // land before the second <li> gets a stable id we can address by.
    await expect
      .poll(
        async () => {
          const ids = await items.evaluateAll((els) =>
            els.map((e) => (e as HTMLElement).dataset.cardId ?? ''),
          );
          return ids.filter((s) => s.length > 0).length;
        },
        { timeout: 10_000 },
      )
      .toBeGreaterThanOrEqual(2);

    // Capture the initial order via the data-card-id we stamp on each
    // <li>. Reorder is then verified by checking the order flipped.
    const initialIds = await items.evaluateAll((els) =>
      els.map((e) => (e as HTMLElement).dataset.cardId ?? ''),
    );
    expect(initialIds.length).toBeGreaterThanOrEqual(2);

    // Focus the first item and press Alt+ArrowDown to swap it with the
    // second. We clear the trace first so the assertion can be specific
    // about the events caused by this gesture.
    await clearEventTrace(page);
    await items.first().focus();
    await page.keyboard.press('Alt+ArrowDown');

    // The optimistic-sort mutation is synchronous in the cache; the
    // server ack and `card.updated` arrive asynchronously. Wait for
    // both halves so the assertion is order-independent. The poll
    // tolerates a longer settle time than the default (10s vs 5s) to
    // absorb the round-trip on a busy CI worker; the swap itself is
    // optimistic so locally it lands in a single frame.
    await waitForEvent(page, 'card.updated');
    await expect
      .poll(
        async () => {
          const ids = await items.evaluateAll((els) =>
            els.map((e) => (e as HTMLElement).dataset.cardId ?? ''),
          );
          return ids[0] === initialIds[1] && ids[1] === initialIds[0];
        },
        { timeout: 10_000 },
      )
      .toBe(true);

    // Home jumps to first; End jumps to last. We assert on the
    // resulting `.is-active` class — applied to whichever item the
    // roving-tabindex hook selected — rather than `toBeFocused`,
    // because the close button inside the active item can briefly
    // capture focus after the optimistic-rerender of the list (the
    // `:focus-within` styling rule exposes the × at full opacity,
    // and an interim re-mount can leave the browser pointing at the
    // child). The `is-active` flag and `aria-checked` state are
    // what AT (and our own assertion contract) consume.
    await items.last().focus();
    await page.keyboard.press('Home');
    await expect(items.first()).toHaveClass(/is-active/);
    await page.keyboard.press('End');
    await expect(items.last()).toHaveClass(/is-active/);

    // Reload — the view-mode overlay must persist, so the page comes
    // back in list view and the cycle button advertises list as current.
    await page.goto(waveUrl);
    const listAfter = page.getByRole('list', { name: /wave cards/i });
    await expect(listAfter).toBeVisible({ timeout: 5_000 });
    const viewButton = page.getByRole('button', {
      name: /^List view — switch to report view$/i,
    });
    await expect(viewButton).toBeVisible();
    await page.locator('body').focus();
    await tabUntil(page, (info) =>
      /^List view — switch to report view$/i.test(info.name ?? ''),
    );
    await expect(viewButton).toBeFocused();
    await page.keyboard.press('Enter');
    await expect(
      page.getByRole('button', {
        name: /^Report view — switch to grid view$/i,
      }),
    ).toBeVisible();
  });

  test('Settings page: all controls reachable and labeled', async ({ page }) => {
    // Tab to the Sidebar's avatar trigger ("Open user menu"). Its
    // accessible name is set explicitly by aria-label. Pressing Enter
    // opens the menu; the first item is Settings, which Enter activates.
    await tabUntil(page, (info) => /open user menu/i.test(info.name ?? ''));
    await page.keyboard.press('Enter');
    await page.keyboard.press('Enter');
    await expect(page).toHaveURL(/\/calm\/settings(\?|$)/);

    // The Settings page is a single <form> with two labeled inputs and
    // two buttons. We assert each control resolves via role + name.
    await expect(
      page.getByRole('textbox', { name: /http proxy/i }).first(),
    ).toBeVisible();
    await expect(
      page.getByRole('textbox', { name: /https proxy/i }).first(),
    ).toBeVisible();
    // Reset starts disabled (no edits yet); Save also starts disabled
    // (form is clean). We just assert they exist and are reachable.
    await expect(page.getByRole('button', { name: /^reset$/i })).toBeVisible();
    await expect(page.getByRole('button', { name: /^save$/i })).toBeVisible();

    // Keyboard reachability: Tab from the document body lands on the
    // first input (skipping anything ahead of it in the focus order is
    // the browser's job; we just verify "can a keyboard user enter this
    // form"). Reset focus to body first so we start from a known place.
    await page.locator('body').focus();
    await tabUntil(page, (info) => /http proxy/i.test(info.name ?? ''));
    // …and Tab again lands on the second input (or the reset button if
    // the layout reorders; we just require *some* keyboard path to it).
    await tabUntil(page, (info) => /https proxy/i.test(info.name ?? ''));
  });
});
