// Headed 验收 demo for terminal-card TUI hosting (mouse-report wheel + OSC 52).
//
// Prereq: `make dev` at http://localhost:4041 (default seed user owner/dev).
// Run headed so you can watch the split pane:
//   cd web && npx playwright test e2e/tui-host-demo.spec.ts --project=chromium --headed
//
// The fixture is a grok-shaped two-pane TUI, not grok itself. After this
// spec is green, the human 验收 is: open a terminal card, run `grok`,
// hover the content pane and scroll, then `y` to copy a block.

import * as path from 'node:path';
import { fileURLToPath } from 'node:url';
import { expect, test, type Page } from '@playwright/test';
import { LIGHT_THEME_RGB } from '../src/api/themeRgb';
import { seedWaveViewMode } from './helpers/reset';

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

test('split-pane TUI wheel reports the content pane and OSC 52 hits the clipboard', async ({
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
  const coveRes = await page.request.post('/api/coves', {
    data: { name: `E2E tui-host demo ${suffix}`, color: '#6a8' },
    headers: { 'content-type': 'application/json' },
  });
  if (!coveRes.ok()) {
    throw new Error(`POST /api/coves failed: ${coveRes.status()}`);
  }
  const cove = (await coveRes.json()) as { id: string };
  const title = `E2E tui-host ${suffix}`;
  const waveRes = await page.request.post('/api/waves', {
    data: {
      cove_id: cove.id,
      title,
      cwd: `/tmp/playwright-tui-host-${cove.id}`,
      attach_folder: true,
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
    headers: { 'content-type': 'application/json' },
  });
  if (!waveRes.ok()) {
    throw new Error(`POST /api/waves failed: ${waveRes.status()}`);
  }
  const wave = (await waveRes.json()) as { id: string };
  await seedWaveViewMode(page.request, wave.id, 'grid');
  await page.goto(`/calm/wave/${wave.id}?testMounts=1`);
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
    `/api/waves/${wave.id}/terminal-cards`,
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

  const terminalId = await expect
    .poll(
      async () => {
        const ids = await page.evaluate(() => {
          const w = window as unknown as { __xtermDumps__?: XtermDumps };
          return Object.keys(w.__xtermDumps__ ?? {});
        });
        return ids.find((id) => !dumpsBefore.includes(id)) ?? '';
      },
      { timeout: 15_000, message: 'new xterm dump hook' },
    )
    .then((id) => id);

  await expect
    .poll(() => dumpTerminal(page, terminalId), {
      timeout: 15_000,
      message: 'demo TUI should paint READY',
    })
    .toContain(READY_MARKER);

  await expect
    .poll(() => page.evaluate(() => navigator.clipboard.readText()), {
      timeout: 8_000,
      message: 'OSC 52 should land in the browser clipboard',
    })
    .toBe(COPY_MARKER);

  const xterm = page.locator('.term.live .xterm-view').first();
  const box = await xterm.boundingBox();
  if (!box) throw new Error('xterm view has no box');
  const scrollBefore = await page.locator('.scroll').evaluate((el) => el.scrollTop);
  await page.mouse.move(box.x + box.width * 0.75, box.y + box.height * 0.45);
  await page.mouse.wheel(0, 240);

  await expect
    .poll(() => dumpTerminal(page, terminalId), {
      timeout: 8_000,
      message: 'wheel over the right pane should report content, not conversation',
    })
    .toMatch(/WHEEL pane=content/);

  const scrollAfter = await page.locator('.scroll').evaluate((el) => el.scrollTop);
  expect(scrollAfter, 'card wheel must not leak to the page').toBe(scrollBefore);
});
