import { expect, test, type Locator, type Page } from '@playwright/test';

test.setTimeout(90_000);

async function blockFonts(page: Page): Promise<void> {
  await page.route('**://fonts.googleapis.com/**', (route) => route.abort());
  await page.route('**://fonts.gstatic.com/**', (route) => route.abort());
}

async function createCove(page: Page): Promise<string> {
  const suffix = Date.now();
  const coveRes = await page.request.post('/api/coves', {
    data: { name: `E2E wheel wave switch cove ${suffix}`, color: '#6a8' },
    headers: { 'content-type': 'application/json' },
  });
  if (!coveRes.ok()) {
    throw new Error(`POST /api/coves failed: ${coveRes.status()}`);
  }
  const cove = (await coveRes.json()) as { id: string };
  return cove.id;
}

async function createWaveInCove(
  page: Page,
  coveId: string,
  titleSuffix: string,
): Promise<{ id: string; title: string }> {
  const title = `E2E wheel wave switch ${titleSuffix}`;
  const cwdSuffix = titleSuffix.replace(/[^a-zA-Z0-9_-]+/g, '-');
  const waveRes = await page.request.post('/api/waves', {
    data: {
      cove_id: coveId,
      title,
      cwd: `/tmp/playwright-wave-switch-${coveId}-${cwdSuffix}`,
      attach_folder: true,
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
    headers: { 'content-type': 'application/json' },
  });
  if (!waveRes.ok()) {
    throw new Error(`POST /api/waves failed: ${waveRes.status()}`);
  }
  const wave = (await waveRes.json()) as { id: string };
  return { id: wave.id, title };
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

test('terminal scrollback persists and wheel works after wave switch', async ({
  page,
}) => {
  await blockFonts(page);
  await page.goto('/calm/', { waitUntil: 'domcontentloaded' });

  const runId = Date.now();
  const coveId = await createCove(page);
  const waveA = await createWaveInCove(page, coveId, `A ${runId}`);
  const waveB = await createWaveInCove(page, coveId, `B ${runId}`);

  await page.goto(`/calm/wave/${waveA.id}?testMounts=1`, {
    waitUntil: 'domcontentloaded',
  });
  await expect(page).toHaveURL(new RegExp(`/calm/wave/${waveA.id}`));
  await expect(page.getByText(waveA.title, { exact: false }).first()).toBeVisible();

  const xterm = await openFreshTerminal(page);
  const terminalIdA = await xterm.getAttribute('data-terminal-id');
  if (!terminalIdA) throw new Error('terminal xterm missing data-terminal-id');

  await xterm.click();
  await page.keyboard.type('cat /etc/services');
  await page.keyboard.press('Enter');
  await expect
    .poll(() => dumpTerminal(page, terminalIdA), {
      timeout: 15_000,
      message: 'terminal should receive /etc/services output',
    })
    .toContain('ftp');

  const viewport = xterm.locator('.xterm-viewport');
  await expect
    .poll(
      () =>
        viewport.evaluate((el) => el.scrollHeight - el.clientHeight > 100),
      {
        timeout: 15_000,
        message: 'terminal should have scrollback',
      },
    )
    .toBe(true);

  const beforeMetrics = await viewport.evaluate((el) => ({
    clientHeight: el.clientHeight,
    scrollHeight: el.scrollHeight,
    scrollTop: el.scrollTop,
  }));
  const heightBefore = beforeMetrics.scrollHeight;
  expect(
    Math.abs(
      beforeMetrics.scrollTop -
        (beforeMetrics.scrollHeight - beforeMetrics.clientHeight),
    ),
  ).toBeLessThanOrEqual(2);

  const waveButton = (title: string) =>
    page.locator('button.side-wave').filter({ hasText: title }).first();

  await expect(waveButton(waveB.title)).toBeVisible();
  await waveButton(waveB.title).click();
  await expect(page).toHaveURL(new RegExp(`/calm/wave/${waveB.id}`));

  await expect(waveButton(waveA.title)).toBeVisible();
  await waveButton(waveA.title).click();
  await expect(page).toHaveURL(new RegExp(`/calm/wave/${waveA.id}`));

  const restoredXterm = page.locator(
    `.term.live .xterm-view[data-terminal-id="${terminalIdA}"]`,
  );
  await expect(restoredXterm).toBeVisible();
  expect(await restoredXterm.getAttribute('data-terminal-id')).toBe(terminalIdA);

  const restoredViewport = restoredXterm.locator('.xterm-viewport');
  await expect
    .poll(() => restoredViewport.evaluate((el) => el.scrollHeight), {
      timeout: 15_000,
      message: 'terminal scrollback height should persist',
    })
    .toBe(heightBefore);

  await addOuterScrollSpace(page);
  await restoredViewport.evaluate((el, scrollTop) => {
    el.scrollTop = scrollTop;
  }, heightBefore / 2);
  await page.locator('.scroll').evaluate((el) => {
    el.scrollTop = 300;
  });

  const beforeOuter = await outerScrollTop(page);
  const beforeViewport = await xtermViewportTop(restoredXterm);
  expect(beforeOuter).toBeGreaterThan(0);
  expect(beforeViewport).toBeGreaterThan(0);
  await wheelOver(page, restoredViewport, -300);

  await expect
    .poll(() => xtermViewportTop(restoredXterm))
    .toBeLessThan(beforeViewport);
  expect(await outerScrollTop(page)).toBe(beforeOuter);
});
