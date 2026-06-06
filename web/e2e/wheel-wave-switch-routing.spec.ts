import { expect, test, type Locator, type Page } from '@playwright/test';

test.setTimeout(90_000);

async function blockFonts(page: Page): Promise<void> {
  await page.route('**://fonts.googleapis.com/**', (route) => route.abort());
  await page.route('**://fonts.gstatic.com/**', (route) => route.abort());
}

async function login(page: Page): Promise<void> {
  const res = await page.request.post('/api/auth/login', {
    data: {
      username: process.env.PROBE_USERNAME ?? 'owner',
      password: process.env.PROBE_PASSWORD ?? 'dev',
    },
  });
  if (!res.ok()) throw new Error(`login failed: ${res.status()} ${await res.text()}`);
}

async function createCove(page: Page): Promise<string> {
  const suffix = Date.now();
  const res = await page.request.post('/api/coves', {
    data: { name: `E2E wheel routing cove ${suffix}`, color: '#6a8' },
    headers: { 'content-type': 'application/json' },
  });
  if (!res.ok()) throw new Error(`POST /api/coves failed: ${res.status()}`);
  return ((await res.json()) as { id: string }).id;
}

async function createWave(
  page: Page,
  coveId: string,
  suffix: string,
): Promise<{ id: string; title: string }> {
  const title = `E2E wheel routing ${suffix}`;
  const cwdSuffix = suffix.replace(/[^a-zA-Z0-9_-]+/g, '-');
  const res = await page.request.post('/api/waves', {
    data: {
      cove_id: coveId,
      title,
      cwd: `/tmp/playwright-wheel-routing-${coveId}-${cwdSuffix}`,
      attach_folder: true,
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
    headers: { 'content-type': 'application/json' },
  });
  if (!res.ok()) throw new Error(`POST /api/waves failed: ${res.status()}`);
  return { id: ((await res.json()) as { id: string }).id, title };
}

async function dumpTerminal(page: Page, terminalId: string): Promise<string> {
  return page.evaluate((id) => {
    const w = window as unknown as {
      __xtermDumps__?: Record<string, () => string>;
    };
    return w.__xtermDumps__?.[id]?.() ?? '';
  }, terminalId);
}

async function openTerminal(page: Page): Promise<Locator> {
  await page.getByRole('button', { name: /^\s*\+?\s*add(\s|$)/i }).first().click();
  await page.getByRole('menuitem', { name: /terminal/i }).click();
  const xterm = page.locator('.term.live .xterm-view').first();
  await expect(xterm).toBeVisible({ timeout: 15_000 });
  return xterm;
}

async function outerScrollTop(page: Page): Promise<number> {
  return page.locator('.scroll').evaluate((el) => el.scrollTop);
}

async function wheelOver(page: Page, target: Locator, deltaY: number): Promise<void> {
  const box = await target.boundingBox();
  if (!box) throw new Error('wheel target has no bounding box');
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.wheel(0, deltaY);
}

test('terminal wheel routes to restored xterm scrollback after wave switch', async ({
  page,
}) => {
  await page.setViewportSize({ width: 1280, height: 1600 });
  await blockFonts(page);
  await page.goto('/calm/', { waitUntil: 'domcontentloaded' });
  await login(page);
  console.log('ok routing booted');

  const runId = Date.now();
  const coveId = await createCove(page);
  const waveA = await createWave(page, coveId, `A ${runId}`);
  const waveB = await createWave(page, coveId, `B ${runId}`);
  console.log('ok routing created waves');

  await page.goto(`/calm/wave/${waveA.id}?testMounts=1`, {
    waitUntil: 'domcontentloaded',
  });
  await expect(page).toHaveURL(new RegExp(`/calm/wave/${waveA.id}`));
  await expect(page.getByText(waveA.title, { exact: false }).first()).toBeVisible();

  const xterm = await openTerminal(page);
  const terminalId = await xterm.getAttribute('data-terminal-id');
  if (!terminalId) throw new Error('terminal xterm missing data-terminal-id');
  await expect
    .poll(() => dumpTerminal(page, terminalId), { timeout: 15_000 })
    .not.toBe('');

  await xterm.click();
  await page.keyboard.type(
    'i=0; while [ $i -lt 200 ]; do echo wheel-routing-$i; i=$((i+1)); done',
  );
  await page.keyboard.press('Enter');
  await expect
    .poll(() => dumpTerminal(page, terminalId), { timeout: 15_000 })
    .toContain('wheel-routing-199');
  console.log('ok routing generated scrollback');

  const waveButton = (title: string) =>
    page.locator('button.side-wave').filter({ hasText: title }).first();

  await waveButton(waveB.title).click();
  await expect(page).toHaveURL(new RegExp(`/calm/wave/${waveB.id}`));
  await waveButton(waveA.title).click();
  await expect(page).toHaveURL(new RegExp(`/calm/wave/${waveA.id}`));
  console.log('ok routing switched waves');

  const restored = page.locator(
    `.term.live .xterm-view[data-terminal-id="${terminalId}"]`,
  );
  await expect(restored).toBeVisible();
  const screen = restored.locator('.xterm-screen');
  await expect
    .poll(() => screen.innerText(), { timeout: 15_000 })
    .toContain('wheel-routing-199');
  await page.waitForTimeout(500);
  await restored.scrollIntoViewIfNeeded();

  const box = await screen.boundingBox();
  if (!box) throw new Error('restored xterm screen has no bounding box');
  const centerY = box.y + box.height / 2;
  expect(centerY).toBeGreaterThanOrEqual(0);
  expect(centerY).toBeLessThanOrEqual(1600);

  const beforeOuter = await outerScrollTop(page);
  const beforeText = await screen.innerText();
  await wheelOver(page, screen, -300);
  await page.waitForTimeout(250);

  expect(await outerScrollTop(page)).toBe(beforeOuter);
  expect(await screen.innerText()).not.toBe(beforeText);
  console.log('ok routing wheel asserted');
});
