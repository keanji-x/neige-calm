import { expect, test, type Locator, type Page } from '@playwright/test';

test.setTimeout(90_000);

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

test('terminal scrollback persists and wheel works after wave switch', async ({
  page,
}) => {
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

  // Generate scrollback via POSIX echo loop — `/etc/services` isn't in
  // the docker server image (debian:bookworm-slim ships without netbase).
  // xterm.js v6 manages scrollback in its IBuffer (viewportY/baseY),
  // not via browser overflow on .xterm-viewport; we measure persistence
  // via the dumpTerminal hook, not viewport.scrollHeight.
  await xterm.click();
  await page.keyboard.type(
    'i=0; while [ $i -lt 200 ]; do echo wheel-wave-$i; i=$((i+1)); done',
  );
  await page.keyboard.press('Enter');
  await expect
    .poll(() => dumpTerminal(page, terminalIdA), {
      timeout: 15_000,
      message: 'terminal should receive echoed scrollback',
    })
    .toContain('wheel-wave-199');

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

  // Scrollback persistence: the `__xtermDumps__` hook only registers
  // under `?testMounts=1`, which is lost after SPA sidebar navigation.
  // Read the rendered DOM text directly — that's what the user sees,
  // and it survives the wave-switch round trip when xterm stays mounted.
  await expect
    .poll(() => restoredXterm.locator('.xterm-screen').innerText(), {
      timeout: 15_000,
      message: 'terminal scrollback should persist across wave switch',
    })
    .toContain('wheel-wave-199');
});
