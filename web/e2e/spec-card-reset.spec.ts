import { test, expect, type Page, type WebSocket } from '@playwright/test';

type TrackedSocket = {
  ws: WebSocket;
  url: string;
  sent: string[];
  closed: boolean;
};

function trackWebSockets(page: Page): TrackedSocket[] {
  const sockets: TrackedSocket[] = [];
  page.on('websocket', (ws) => {
    const tracked: TrackedSocket = {
      ws,
      url: ws.url(),
      sent: [],
      closed: false,
    };
    sockets.push(tracked);
    ws.on('framesent', (frame) => {
      tracked.sent.push(String(frame.payload));
    });
    ws.on('close', () => {
      tracked.closed = true;
    });
  });
  return sockets;
}

function terminalSockets(
  sockets: TrackedSocket[],
  terminalId: string,
): TrackedSocket[] {
  const suffix = `/api/terminals/${encodeURIComponent(terminalId)}`;
  return sockets.filter((s) => s.url.endsWith(suffix));
}

test.setTimeout(60_000);

test('spec-card-reset confirms, posts reset, and reconnects the spec terminal', async ({
  page,
}) => {
  await page.route('**://fonts.googleapis.com/**', (route) => route.abort());
  await page.route('**://fonts.gstatic.com/**', (route) => route.abort());

  const sockets = trackWebSockets(page);
  const resetRequests: string[] = [];
  let terminalIdForMock = '';

  await page.route(/\/api\/cards\/[^/]+\/spec\/reset$/, async (route) => {
    const url = route.request().url();
    resetRequests.push(url);
    const cardId = decodeURIComponent(
      new URL(url).pathname.match(/\/api\/cards\/([^/]+)\/spec\/reset$/)?.[1] ??
        'card_unknown',
    );
    await route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify({
        card_id: cardId,
        terminal_id: terminalIdForMock,
        new_thread_id: 'thread_e2e_reset',
      }),
    });
  });

  await page.goto('/calm/', { waitUntil: 'domcontentloaded' });

  const coveRes = await page.request.post('/api/coves', {
    data: { name: `E2E reset cove ${Date.now()}`, color: '#6a8' },
    headers: { 'content-type': 'application/json' },
  });
  if (!coveRes.ok()) {
    const body = await coveRes.text().catch(() => '<unreadable>');
    throw new Error(
      `POST /api/coves -> ${coveRes.status()} ${coveRes.statusText()}: ${body}`,
    );
  }
  const cove = (await coveRes.json()) as { id: string };

  const waveTitle = `E2E spec-card-reset ${Date.now()}`;
  const waveRes = await page.request.post('/api/waves', {
    data: {
      cove_id: cove.id,
      title: waveTitle,
      cwd: `/tmp/playwright-reset-${cove.id}`,
      attach_folder: true,
      theme: { fg: [216, 219, 226], bg: [15, 20, 24] },
    },
    headers: { 'content-type': 'application/json' },
  });
  if (!waveRes.ok()) {
    const body = await waveRes.text().catch(() => '<unreadable>');
    throw new Error(
      `POST /api/waves -> ${waveRes.status()} ${waveRes.statusText()}: ${body}`,
    );
  }
  const wave = (await waveRes.json()) as { id: string };

  await page.goto(`/calm/wave/${wave.id}?testMounts=1`, {
    waitUntil: 'domcontentloaded',
  });
  await expect(page).toHaveURL(/\/calm\/wave\/[^/]+\?testMounts=1$/);
  await expect(page.getByText(waveTitle, { exact: false }).first()).toBeVisible();

  const specCard = page.locator('.codex-card').first();
  await expect(specCard).toBeVisible({ timeout: 15_000 });
  await expect(
    specCard.getByRole('button', { name: 'Refresh terminal' }),
  ).toBeVisible({ timeout: 15_000 });
  const reset = specCard.getByRole('button', { name: 'Reset spec session' });
  await expect(reset).toBeVisible({ timeout: 15_000 });

  const xterm = specCard.locator('.xterm-view').first();
  await expect(xterm).toBeVisible({ timeout: 15_000 });
  const terminalId = await xterm.getAttribute('data-terminal-id');
  if (!terminalId) throw new Error('spec card XtermView missing data-terminal-id');
  terminalIdForMock = terminalId;

  await expect
    .poll(() => terminalSockets(sockets, terminalId).length, {
      timeout: 15_000,
      message: 'initial terminal WebSocket should open',
    })
    .toBeGreaterThanOrEqual(1);
  const initialSocket = terminalSockets(sockets, terminalId).at(-1)!;

  await reset.click();
  await expect(
    page.getByRole('dialog', { name: 'Reset spec session?' }),
  ).toBeVisible();
  await page.getByRole('button', { name: 'Cancel' }).click();
  await expect(
    page.getByRole('dialog', { name: 'Reset spec session?' }),
  ).toBeHidden();
  expect(resetRequests).toEqual([]);

  const beforeCount = terminalSockets(sockets, terminalId).length;
  await reset.click();
  await page.getByRole('button', { name: 'Reset session' }).click();

  await expect
    .poll(() => resetRequests.length, {
      timeout: 15_000,
      message: 'confirm should POST the reset endpoint',
    })
    .toBe(1);
  await expect
    .poll(() => terminalSockets(sockets, terminalId).length, {
      timeout: 15_000,
      message: 'successful reset should refresh the terminal WebSocket',
    })
    .toBeGreaterThan(beforeCount);

  const resetSocket = terminalSockets(sockets, terminalId).at(-1)!;
  expect(resetSocket.url).toBe(initialSocket.url);
  expect(resetSocket.ws).not.toBe(initialSocket.ws);
  await expect
    .poll(() => initialSocket.closed, {
      timeout: 15_000,
      message: 'reset refresh should close the old terminal WebSocket',
    })
    .toBe(true);
  await expect
    .poll(
      () => resetSocket.sent.some((frame) => frame.includes('ClientHello')),
      {
        timeout: 15_000,
        message: 'replacement terminal WebSocket should send ClientHello',
      },
    )
    .toBe(true);
});
