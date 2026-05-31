import { test, expect, type Page, type WebSocket } from '@playwright/test';

type TrackedSocket = {
  ws: WebSocket;
  url: string;
  sent: string[];
  received: string[];
  closed: boolean;
};

function trackWebSockets(page: Page): TrackedSocket[] {
  const sockets: TrackedSocket[] = [];
  page.on('websocket', (ws) => {
    const tracked: TrackedSocket = {
      ws,
      url: ws.url(),
      sent: [],
      received: [],
      closed: false,
    };
    sockets.push(tracked);
    ws.on('framesent', (frame) => {
      tracked.sent.push(String(frame.payload));
    });
    ws.on('framereceived', (frame) => {
      tracked.received.push(String(frame.payload));
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

test('spec-card-refresh reconnects the spec terminal without reset API calls', async ({
  page,
}) => {
  await page.route('**://fonts.googleapis.com/**', (route) => route.abort());
  await page.route('**://fonts.gstatic.com/**', (route) => route.abort());

  const sockets = trackWebSockets(page);
  const resetRequests: string[] = [];
  page.on('request', (request) => {
    const url = request.url();
    if (/\/api\/cards\/[^/]+\/spec\/reset/.test(url)) {
      resetRequests.push(url);
    }
  });

  await page.goto('/calm/', { waitUntil: 'domcontentloaded' });

  const coveRes = await page.request.post('/api/coves', {
    data: { name: `E2E refresh cove ${Date.now()}`, color: '#6a8' },
    headers: { 'content-type': 'application/json' },
  });
  if (!coveRes.ok()) {
    const body = await coveRes.text().catch(() => '<unreadable>');
    throw new Error(
      `POST /api/coves -> ${coveRes.status()} ${coveRes.statusText()}: ${body}`,
    );
  }
  const cove = (await coveRes.json()) as { id: string };

  const waveTitle = `E2E spec-card-refresh ${Date.now()}`;
  const waveRes = await page.request.post('/api/waves', {
    data: {
      cove_id: cove.id,
      title: waveTitle,
      cwd: `/tmp/playwright-refresh-${cove.id}`,
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
  const refresh = specCard.getByRole('button', { name: 'Refresh terminal' });
  await expect(refresh).toBeVisible({ timeout: 15_000 });

  const xterm = specCard.locator('.xterm-view').first();
  await expect(xterm).toBeVisible({ timeout: 15_000 });
  const terminalId = await xterm.getAttribute('data-terminal-id');
  if (!terminalId) throw new Error('spec card XtermView missing data-terminal-id');

  await expect
    .poll(() => terminalSockets(sockets, terminalId).length, {
      timeout: 15_000,
      message: 'initial terminal WebSocket should open',
    })
    .toBeGreaterThanOrEqual(1);
  const initialSocket = terminalSockets(sockets, terminalId).at(-1)!;

  // Deliberately NOT asserting ServerHello / render-plane content on
  // either socket. The CI chromium-e2e docker stack ships without
  // codex on PATH (see .github/workflows/ci.yml: "chromium-e2e specs
  // don't exercise codex today"), so the spec daemon never produces a
  // ServerHello frame in CI even though the WS opens cleanly. The
  // Refresh button's contract is purely client-side: bump
  // reconnectKey -> tear down old WS -> open a new WS to the same
  // URL with a fresh ClientHello. That's what we verify below. The
  // server-side render-plane replay (ServerHello + RenderSnapshot on
  // lag) is covered by `client_pump` unit tests + the user's
  // hands-on preview test, both of which run against a stack that
  // does have codex available.

  const beforeCount = terminalSockets(sockets, terminalId).length;
  await refresh.click();

  await expect
    .poll(() => terminalSockets(sockets, terminalId).length, {
      timeout: 15_000,
      message: 'refresh should open a replacement terminal WebSocket',
    })
    .toBeGreaterThan(beforeCount);
  const refreshedSocket = terminalSockets(sockets, terminalId).at(-1)!;

  expect(refreshedSocket.url).toBe(initialSocket.url);
  expect(refreshedSocket.ws).not.toBe(initialSocket.ws);
  await expect
    .poll(() => initialSocket.closed, {
      timeout: 15_000,
      message: 'refresh should close the old terminal WebSocket',
    })
    .toBe(true);
  await expect
    .poll(
      () => refreshedSocket.sent.some((frame) => frame.includes('ClientHello')),
      {
        timeout: 15_000,
        message: 'replacement terminal WebSocket should send ClientHello',
      },
    )
    .toBe(true);

  expect(resetRequests).toEqual([]);
});
