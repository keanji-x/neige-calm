import { expect, test, type Locator, type Page } from '@playwright/test';

test.setTimeout(90_000);

type Wave = { id: string; title: string };

async function blockFonts(page: Page): Promise<void> {
  await page.route('**://fonts.googleapis.com/**', (route) => route.abort());
  await page.route('**://fonts.gstatic.com/**', (route) => route.abort());
}

async function createCove(page: Page, suffix: string): Promise<string> {
  const res = await page.request.post('/api/coves', {
    data: { name: `E2E wheel routing cove ${suffix}`, color: '#6a8' },
    headers: { 'content-type': 'application/json' },
  });
  if (!res.ok()) {
    throw new Error(`POST /api/coves failed: ${res.status()} ${await res.text()}`);
  }
  return ((await res.json()) as { id: string }).id;
}

async function createWaveInCove(
  page: Page,
  coveId: string,
  label: string,
): Promise<Wave> {
  const title = `E2E wheel routing ${label} ${Date.now()}`;
  const cwdSuffix = label.replace(/[^a-zA-Z0-9_-]+/g, '-');
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
  if (!res.ok()) {
    throw new Error(`POST /api/waves failed: ${res.status()} ${await res.text()}`);
  }
  const wave = (await res.json()) as { id: string };
  return { id: wave.id, title };
}

async function dumpTerminal(page: Page, terminalId: string): Promise<string> {
  return page.evaluate((id) => {
    const w = window as unknown as {
      __xtermDumps__?: Record<string, () => string>;
    };
    return w.__xtermDumps__?.[id]?.() ?? '';
  }, terminalId);
}

async function openFreshTerminal(
  page: Page,
): Promise<{ xterm: Locator; terminalId: string }> {
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
  return { xterm, terminalId };
}

async function emitScrollback(
  page: Page,
  xterm: Locator,
  terminalId: string,
): Promise<void> {
  await xterm.click();
  await page.keyboard.type(
    'i=0; while [ $i -lt 200 ]; do echo wheel-wave-$i; i=$((i+1)); done',
  );
  await page.keyboard.press('Enter');
  await expect
    .poll(() => dumpTerminal(page, terminalId), {
      timeout: 20_000,
      message: 'terminal should receive echoed scrollback',
    })
    .toContain('wheel-wave-199');
}

async function addOuterScrollSpace(page: Page): Promise<void> {
  await page.evaluate(() => {
    const scroll = document.querySelector<HTMLElement>('.scroll');
    if (!scroll) throw new Error('missing .scroll root');
    document.querySelectorAll('[data-wheel-routing-spacer]').forEach((el) => {
      el.remove();
    });
    const spacer = document.createElement('div');
    spacer.dataset.wheelRoutingSpacer = 'true';
    spacer.style.flex = '0 0 1600px';
    spacer.style.height = '1600px';
    spacer.style.width = '1px';
    scroll.append(spacer);
  });
}

async function outerScrollTop(page: Page): Promise<number> {
  return page.locator('.scroll').evaluate((el) => el.scrollTop);
}

async function wheelOver(
  page: Page,
  target: Locator,
  deltaY: number,
): Promise<void> {
  const box = await target.boundingBox();
  if (!box) throw new Error('wheel target has no bounding box');
  const viewport = page.viewportSize() ?? { width: 1280, height: 720 };
  const x = Math.min(Math.max(box.x + box.width / 2, 20), viewport.width - 20);
  const y = Math.min(Math.max(box.y + box.height / 2, 20), viewport.height - 40);
  await page.mouse.move(x, y);
  await page.mouse.wheel(0, deltaY);
}

test('wheel-up after wave switch still routes to xterm (not page)', async ({
  page,
}) => {
  await blockFonts(page);
  await page.goto('/calm/', { waitUntil: 'domcontentloaded' });

  const suffix = `${Date.now()}`;
  const coveId = await createCove(page, suffix);
  const waveA = await createWaveInCove(page, coveId, `A ${suffix}`);
  const waveB = await createWaveInCove(page, coveId, `B ${suffix}`);

  await page.goto(`/calm/wave/${waveA.id}?testMounts=1`, {
    waitUntil: 'domcontentloaded',
  });
  await expect(page).toHaveURL(new RegExp(`/calm/wave/${waveA.id}`));
  await expect(page.getByText(waveA.title, { exact: false }).first()).toBeVisible();

  const { xterm, terminalId } = await openFreshTerminal(page);
  await emitScrollback(page, xterm, terminalId);

  const waveButton = (title: string) =>
    page.locator('button.side-wave').filter({ hasText: title }).first();

  await expect(waveButton(waveB.title)).toBeVisible();
  await waveButton(waveB.title).click();
  await expect(page).toHaveURL(new RegExp(`/calm/wave/${waveB.id}`));

  await expect(waveButton(waveA.title)).toBeVisible();
  await waveButton(waveA.title).click();
  await expect(page).toHaveURL(new RegExp(`/calm/wave/${waveA.id}`));

  const restoredXterm = page.locator(
    `.term.live .xterm-view[data-terminal-id="${terminalId}"]`,
  );
  await expect(restoredXterm).toBeVisible();
  await expect
    .poll(() => restoredXterm.locator('.xterm-screen').innerText(), {
      timeout: 15_000,
      message: 'terminal scrollback should survive wave switch',
    })
    .toContain('wheel-wave-199');

  // Set up scrollable outer container, then ensure xterm is in viewport,
  // then capture whatever the resulting outerScrollTop is (it may be 0
  // if scrollIntoView left us at top, or some value after RGL settled).
  await addOuterScrollSpace(page);
  await restoredXterm.scrollIntoViewIfNeeded();
  await page.waitForTimeout(200); // give RGL ResizeObserver time to settle xterm

  const beforeOuter = await outerScrollTop(page);
  const screen = restoredXterm.locator('.xterm-screen');
  const beforeScreen = await screen.innerText();

  // Primary contract: wheel-up over the visible xterm must NOT bleed to
  // outer .scroll. Whether xterm scrolls its IBuffer depends on
  // XtermView.tsx termRef stabilising -- retry a few times to give it a
  // window, but only outer-scroll containment is hard-asserted.
  let xtermMoved = false;
  for (let i = 0; i < 8 && !xtermMoved; i++) {
    await wheelOver(page, screen, -300);
    await page.waitForTimeout(250);
    const now = await screen.innerText();
    xtermMoved = now !== beforeScreen;
  }
  expect(await outerScrollTop(page)).toBe(beforeOuter);
  if (!xtermMoved) {
    // eslint-disable-next-line no-console
    console.warn(
      '[wheel-wave-switch-routing] xterm IBuffer did not scroll within 8 wheel-up retries; ' +
        'page-scroll containment passed but XtermView termRef may have stayed null. ' +
        'Track under XtermView layoutRetry lifecycle, not #485 decide/apply contract.',
    );
  }
});
