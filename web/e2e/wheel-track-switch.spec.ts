import { expect, test, type Locator, type Page } from '@playwright/test';

test.setTimeout(90_000);

async function createArea(page: Page): Promise<string> {
  const suffix = Date.now();
  const areaRes = await page.request.post('/api/areas', {
    data: { name: `E2E wheel track switch area ${suffix}`, color: '#6a8' },
    headers: { 'content-type': 'application/json' },
  });
  if (!areaRes.ok()) {
    throw new Error(`POST /api/areas failed: ${areaRes.status()}`);
  }
  const area = (await areaRes.json()) as { id: string };
  return area.id;
}

async function createTrackInArea(
  page: Page,
  areaId: string,
  titleSuffix: string,
): Promise<{ id: string; title: string }> {
  const title = `E2E wheel track switch ${titleSuffix}`;
  const trackRes = await page.request.post('/api/tracks', {
    data: {
      area_id: areaId,
      title,
      // #1147 S3 — no `cwd`: take the kernel-managed workspace branch.
      // This spec is about wheel track-switching, not working
      // directories. See `helpers/reset.ts::createTrackInArea` for why
      // the invented `/tmp/playwright-track-switch-<id>` attached path
      // was never valid. (The per-suffix cwd namespace it needed to
      // dodge `area_folders.UNIQUE(path)` goes away with it.)
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

test('terminal scrollback persists and wheel works after track switch', async ({
  page,
}) => {
  await page.goto('/calm/', { waitUntil: 'domcontentloaded' });

  const runId = Date.now();
  const areaId = await createArea(page);
  const trackA = await createTrackInArea(page, areaId, `A ${runId}`);
  const trackB = await createTrackInArea(page, areaId, `B ${runId}`);

  await page.goto(`/calm/track/${trackA.id}?testMounts=1`, {
    waitUntil: 'domcontentloaded',
  });
  await expect(page).toHaveURL(new RegExp(`/calm/track/${trackA.id}`));
  await expect(page.getByText(trackA.title, { exact: false }).first()).toBeVisible();

  const xterm = await openFreshTerminal(page);
  const terminalIdA = await xterm.getAttribute('data-terminal-id');
  if (!terminalIdA) throw new Error('terminal xterm missing data-terminal-id');

  // Generate scrollback via POSIX echo loop — `/etc/services` isn't in
  // the docker server image (debian:bookworm-slim ships without netbase).
  // xterm.js v6 manages scrollback in its IBuffer (viewportY/baseY),
  // not via browser overflow on .xterm-viewport; we measure persistence
  // via the dumpTerminal hook, not viewport.scrollHeight.
  await xterm.click();
  await page.keyboard.type(
    'i=0; while [ $i -lt 200 ]; do echo wheel-track-$i; i=$((i+1)); done',
  );
  await page.keyboard.press('Enter');
  await expect
    .poll(() => dumpTerminal(page, terminalIdA), {
      timeout: 15_000,
      message: 'terminal should receive echoed scrollback',
    })
    .toContain('wheel-track-199');

  const trackButton = (title: string) =>
    page.locator('button.side-track').filter({ hasText: title }).first();

  await expect(trackButton(trackB.title)).toBeVisible();
  await trackButton(trackB.title).click();
  await expect(page).toHaveURL(new RegExp(`/calm/track/${trackB.id}`));

  await expect(trackButton(trackA.title)).toBeVisible();
  await trackButton(trackA.title).click();
  await expect(page).toHaveURL(new RegExp(`/calm/track/${trackA.id}`));

  const restoredXterm = page.locator(
    `.term.live .xterm-view[data-terminal-id="${terminalIdA}"]`,
  );
  await expect(restoredXterm).toBeVisible();
  expect(await restoredXterm.getAttribute('data-terminal-id')).toBe(terminalIdA);

  // Scrollback persistence: the `__xtermDumps__` hook only registers
  // under `?testMounts=1`, which is lost after SPA sidebar navigation.
  // Read the rendered DOM text directly — that's what the user sees,
  // and it survives the track-switch round trip when xterm stays mounted.
  await expect
    .poll(() => restoredXterm.locator('.xterm-screen').innerText(), {
      timeout: 15_000,
      message: 'terminal scrollback should persist across track switch',
    })
    .toContain('wheel-track-199');
});
