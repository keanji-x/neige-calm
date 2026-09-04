// Headed 验收 demo for terminal-card TUI hosting (mouse-report wheel + OSC 52).
//
// Prereq: `make dev` at http://localhost:4041 (default seed user owner/dev).
// Run headed so you can watch the split pane:
//   cd web && npx playwright test e2e/tui-host-demo.spec.ts --project=chromium --headed
//
// The fixture is a grok-shaped two-pane TUI, not grok itself. After this
// test is green, the human 验收 is: open a terminal card, run `grok`,
// hover the content pane and scroll, then `y` to copy a block.

import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { expect, test, type Page } from '@playwright/test';
import { LIGHT_THEME_RGB } from '../src/api/themeRgb';
import { seedTrackViewMode } from './helpers/reset';

test.setTimeout(90_000);

const COPY_MARKER = 'neige-osc52-ok';
const READY_MARKER = 'TUI_HOST_DEMO_READY';

type XtermDumps = Record<string, () => string>;

async function login(page: Page): Promise<void> {
  const res = await page.request.post('/api/auth/login', {
    data: {
      username: process.env.PROBE_USERNAME ?? 'owner',
      password: process.env.PROBE_PASSWORD ?? 'dev',
    },
  });
  if (!res.ok()) {
    throw new Error(`login failed: ${res.status()} ${await res.text()}`);
  }
}

async function dumpTerminal(page: Page, terminalId: string): Promise<string> {
  return page.evaluate((id) => {
    const w = window as unknown as { __xtermDumps__?: XtermDumps };
    return w.__xtermDumps__?.[id]?.() ?? '';
  }, terminalId);
}

test('split-pane TUI copies via OSC 52 and card wheel does not scroll the page', async ({
  page,
  context,
}) => {
  await page.setViewportSize({ width: 1280, height: 900 });
  await page.goto('/calm/?testMounts=1', { waitUntil: 'domcontentloaded' });
  await context.grantPermissions(['clipboard-read', 'clipboard-write'], {
    origin: new URL(page.url()).origin,
  });
  await login(page);

  const suffix = Date.now();
  const areaRes = await page.request.post('/api/areas', {
    data: { name: `E2E tui-host demo ${suffix}`, color: '#6a8' },
    headers: { 'content-type': 'application/json' },
  });
  if (!areaRes.ok()) {
    throw new Error(`POST /api/areas failed: ${areaRes.status()}`);
  }
  const area = (await areaRes.json()) as { id: string };
  const title = `E2E tui-host ${suffix}`;
  const trackRes = await page.request.post('/api/tracks', {
    data: {
      area_id: area.id,
      title,
      // #1147 S3 — no `cwd`: take the kernel-managed workspace branch.
      // This test is about the TUI host renderer, not working
      // directories (the terminal card below still passes its own
      // `cwd: '/tmp'`, which is a real directory inside the kernel).
      // See `helpers/reset.ts::createTrackInArea` for why the invented
      // `/tmp/playwright-tui-host-<id>` attached path was never valid.
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
    headers: { 'content-type': 'application/json' },
  });
  if (!trackRes.ok()) {
    throw new Error(`POST /api/tracks failed: ${trackRes.status()}`);
  }
  const track = (await trackRes.json()) as { id: string };
  await seedTrackViewMode(page.request, track.id, 'grid');
  await page.goto(`/calm/track/${track.id}?testMounts=1`);
  await expect(page.getByText(title, { exact: false }).first()).toBeVisible();

  const dumpsBefore = await page.evaluate(() => {
    const w = window as unknown as { __xtermDumps__?: XtermDumps };
    return Object.keys(w.__xtermDumps__ ?? {});
  });
  const fixturePath = path.resolve(
    fileURLToPath(import.meta.url),
    '../fixtures/tui-host-demo.py',
  );
  const cardRes = await page.request.post(
    `/api/tracks/${track.id}/terminal-cards`,
    {
      data: {
        program: `python3 '${fixturePath}'`,
        cwd: '/tmp',
        env: {},
        theme: { fg: LIGHT_THEME_RGB.fg, bg: LIGHT_THEME_RGB.bg },
      },
      headers: { 'content-type': 'application/json' },
    },
  );
  if (!cardRes.ok()) {
    throw new Error(
      `POST terminal-cards -> ${cardRes.status()} ${await cardRes.text()}`,
    );
  }

  let terminalId = '';
  await expect
    .poll(
      async () => {
        const ids = await page.evaluate(() => {
          const w = window as unknown as { __xtermDumps__?: XtermDumps };
          return Object.keys(w.__xtermDumps__ ?? {});
        });
        const id = ids.find((found) => !dumpsBefore.includes(found)) ?? '';
        if (id) terminalId = id;
        return id;
      },
      { timeout: 15_000, message: 'new xterm dump hook' },
    )
    .not.toBe('');

  await expect
    .poll(() => dumpTerminal(page, terminalId), {
      timeout: 15_000,
      message: 'demo TUI should paint READY',
    })
    .toContain(READY_MARKER);

  const xterm = page.locator('.term.live .xterm-view').first();
  await expect(xterm).toBeVisible();
  // Startup OSC 52 can race the attach; click + `y` re-emits after a
  // user gesture so Chromium allows clipboard.writeText.
  await xterm.click();
  await page.keyboard.type('y');

  await expect
    .poll(() => dumpTerminal(page, terminalId), {
      timeout: 8_000,
      message: 'demo TUI should confirm the OSC 52 copy',
    })
    .toContain(`COPIED=${COPY_MARKER}`);

  await expect
    .poll(() => page.evaluate(() => navigator.clipboard.readText()), {
      timeout: 8_000,
      message: 'OSC 52 should land in the browser clipboard',
    })
    .toBe(COPY_MARKER);

  // Playwright's synthetic wheel does not always become CSI mouse reports
  // (real pointer wheels do — that's the human 验收). Still pin that the
  // card host does not leak the event to the page scroller.
  const box = await xterm.boundingBox();
  if (!box) throw new Error('xterm view has no box');
  const scrollBefore = await page.locator('.scroll').evaluate((el) => el.scrollTop);
  await page.mouse.move(box.x + box.width * 0.8, box.y + box.height * 0.4);
  await page.mouse.wheel(0, 240);
  const scrollAfter = await page.locator('.scroll').evaluate((el) => el.scrollTop);
  expect(scrollAfter, 'card wheel must not leak to the page').toBe(scrollBefore);
});
