import { expect, test, type Locator, type Page } from '@playwright/test';

test.setTimeout(90_000);

async function createTrack(page: Page): Promise<{ id: string; title: string }> {
  const suffix = Date.now();
  const areaRes = await page.request.post('/api/areas', {
    data: { name: `E2E wheel area ${suffix}`, color: '#6a8' },
    headers: { 'content-type': 'application/json' },
  });
  if (!areaRes.ok()) {
    throw new Error(`POST /api/areas failed: ${areaRes.status()}`);
  }
  const area = (await areaRes.json()) as { id: string };
  const title = `E2E wheel edge ${suffix}`;
  const trackRes = await page.request.post('/api/tracks', {
    data: {
      area_id: area.id,
      title,
      // #1147 S3 — no `cwd`: take the kernel-managed workspace branch.
      // This test is about wheel mode, not working directories. See
      // `helpers/reset.ts::createTrackInArea` for why the invented
      // `/tmp/playwright-wheel-<id>` attached path was never valid.
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
    headers: { 'content-type': 'application/json' },
  });
  if (!trackRes.ok()) {
    throw new Error(`POST /api/tracks failed: ${trackRes.status()}`);
  }
  const track = (await trackRes.json()) as { id: string };
  return { id: track.id, title };
}

async function openFreshTerminal(page: Page): Promise<Locator> {
  await page.goto('/calm/', { waitUntil: 'domcontentloaded' });
  const track = await createTrack(page);
  await page.goto(`/calm/track/${track.id}?testMounts=1`, {
    waitUntil: 'domcontentloaded',
  });
  await expect(page.getByText(track.title, { exact: false }).first()).toBeVisible();

  const add = page
    .getByRole('button', { name: /^\s*\+?\s*add(\s|$)/i })
    .first();
  await expect(add).toBeVisible();
  await add.click();
  await page.getByRole('menuitem', { name: /terminal/i }).click();

  const xterm = page.locator('.term.live .xterm-view').first();
  await expect(xterm).toBeVisible({ timeout: 15_000 });
  const terminalId = await xterm.getAttribute('data-terminal-id');
  if (!terminalId) throw new Error('terminal xterm missing data-terminal-id');
  await expect
    .poll(() => dumpTerminal(page, terminalId), {
      timeout: 15_000,
      message: 'terminal should have registered its test dump hook',
    })
    .not.toBe('');
  return xterm;
}

async function dumpTerminal(page: Page, terminalId: string): Promise<string> {
  return page.evaluate((id) => {
    const w = window as unknown as {
      __xtermDumps__?: Record<string, () => string>;
    };
    return w.__xtermDumps__?.[id]?.() ?? '';
  }, terminalId);
}

async function addOuterScrollSpace(page: Page): Promise<void> {
  await page.evaluate(() => {
    const scroll = document.querySelector<HTMLElement>('.scroll');
    if (!scroll) throw new Error('missing .scroll root');
    document.querySelector('[data-wheel-edge-spacer]')?.remove();
    const spacer = document.createElement('div');
    spacer.dataset.wheelEdgeSpacer = 'true';
    spacer.style.flex = '0 0 1600px';
    spacer.style.height = '1600px';
    spacer.style.width = '1px';
    scroll.append(spacer);
    scroll.scrollTop = 0;
  });
}

async function outerScrollTop(page: Page): Promise<number> {
  return page.locator('.scroll').evaluate((el) => el.scrollTop);
}

async function xtermViewportTop(xterm: Locator): Promise<number> {
  return xterm.locator('.xterm-viewport').evaluate((el) => el.scrollTop);
}

async function wheelOver(page: Page, target: Locator, deltaY: number): Promise<void> {
  const box = await target.boundingBox();
  if (!box) throw new Error('wheel target has no bounding box');
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.wheel(0, deltaY);
}

test('empty-buffer top fall-through', async ({ page }) => {
  const xterm = await openFreshTerminal(page);
  await addOuterScrollSpace(page);
  await xterm.locator('.xterm-viewport').scrollIntoViewIfNeeded();

  const beforeOuter = await outerScrollTop(page);
  const beforeViewport = await xtermViewportTop(xterm);
  await wheelOver(page, xterm.locator('.xterm-viewport'), 600);

  await expect.poll(() => outerScrollTop(page)).toBeGreaterThan(beforeOuter);
  expect(await xtermViewportTop(xterm)).toBe(beforeViewport);
});

test('bottom-edge fall-through', async ({ page }) => {
  const xterm = await openFreshTerminal(page);
  const terminalId = await xterm.getAttribute('data-terminal-id');
  if (!terminalId) throw new Error('terminal xterm missing data-terminal-id');

  await xterm.click();
  await page.keyboard.type(
    'i=0; while [ $i -lt 180 ]; do echo wheel-$i; i=$((i+1)); done',
  );
  await page.keyboard.press('Enter');
  await expect
    .poll(() => dumpTerminal(page, terminalId), {
      timeout: 15_000,
      message: 'terminal should receive generated scrollback',
    })
    .toContain('wheel-179');

  // xterm.js v6 manages scrollback in its own IBuffer (viewportY/baseY),
  // not via browser-native overflow scroll on .xterm-viewport
  // (scrollHeight === clientHeight always). The dump-contains assertion
  // above already proves 180 lines were echoed, so the buffer is at the
  // live tail (viewportY === baseY). That's the wheel-down edge we want.
  const viewport = xterm.locator('.xterm-viewport');
  await addOuterScrollSpace(page);
  await viewport.scrollIntoViewIfNeeded();
  const beforeOuter = await outerScrollTop(page);
  await wheelOver(page, viewport, 600);

  await expect.poll(() => outerScrollTop(page)).toBeGreaterThan(beforeOuter);
});
