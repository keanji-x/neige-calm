import { expect, test } from '@playwright/test';

test('shows local screenshots in planner history after reopening and on mobile', async ({ page }, testInfo) => {
  const track = { id: 'image-track', area_id: 'image-area', title: 'Screenshot review', sort: 1,
    cwd: '/checkout', workspace: { worktree: '/work/image-track' },
    pinned_at: null, closed_at: null, created_at: 1, updated_at: 2 };
  const card = { id: 'image-planner', track_id: track.id, kind: 'codex', title: 'Planner', sort: 1,
    payload: { planner_harness: true }, deletable: true, created_at: 1, updated_at: 2 };
  const screenshotPage = await page.context().newPage();
  await screenshotPage.setViewportSize({ width: 1200, height: 800 });
  await screenshotPage.setContent('<main style="padding:40px;background:#eef2f6;height:700px">'
    + '<h1>Screenshot fixture</h1><p>A local screenshot from the planner workspace.</p></main>');
  const screenshot = await screenshotPage.screenshot();
  await screenshotPage.close();
  const rawRequests: string[] = [];
  await page.route('**/api/**', async (route) => {
    const url = new URL(route.request().url());
    const path = url.pathname;
    // Vite also serves source modules under core/api; only mock HTTP API routes.
    if (!path.startsWith('/api/')) { await route.continue(); return; }
    let body: unknown = [];
    if (path === '/api/auth/whoami') body = { userId: 'test', displayName: 'Test', role: 'admin', sessionId: 'test' };
    if (path === '/api/version') body = { webCompatVersion: 39, minWebCompatVersion: 39,
      syncEventVersion: 24, dbInstanceId: 'image-test', databaseId: 'image-test', nowMs: Date.now() };
    if (path === '/api/settings') body = {};
    if (path === '/api/areas') body = [{ id: track.area_id, name: 'Work', color: '#123456', sort: 1,
      kind: 'user', created_at: 1, updated_at: 1 }];
    if (path === `/api/areas/${track.area_id}/tracks`) body = [track];
    if (path === `/api/tracks/${track.id}`) body = { track, can_reopen: false, can_close: true, cards: [card], overlays: [] };
    if (path === `/api/cards/${card.id}/planner/run`) body = { card_id: card.id, worker_session_id: 'image-session',
      phase: 'idle', model: null, reasoning_effort: null, blocked_reason: null, running_turn: null };
    if (path === `/api/cards/${card.id}/harness/items`) body = [{ id: 1, worker_session_id: 'image-session',
      card_id: card.id, track_id: track.id, thread_id: 'image-thread', turn_id: null, turn_error_text: null,
      item_uuid: 'reply', item_type: 'agentMessage', method: 'item/completed', created_at_ms: 1,
      params: JSON.stringify({ item: { text: 'Here is the screenshot.\n\n'
        + '![Screenshot](/work/image-track/screenshots/page.png)\n\n'
        + '![Detail](screenshots/detail.png)\n\n![Missing](screenshots/missing.png)\n\n'
        + '![Outside](/other-track/secret.png)' } }) }];
    if (path === `/api/tracks/${track.id}/workspace/readfile-raw`) {
      rawRequests.push(url.searchParams.get('path') ?? '');
      const missing = url.searchParams.get('path') === 'screenshots/missing.png';
      await route.fulfill({ status: missing ? 404 : 200, contentType: 'image/png', body: missing ? '' : screenshot });
      return;
    }
    await route.fulfill({ json: body });
  });
  await page.routeWebSocket(/.*/, async (socket) => { await socket.close(); });
  await page.goto(`/next/track/${track.id}`);
  await page.getByRole('button', { name: 'Conversation Planner' }).click();
  const image = page.getByRole('img', { name: 'Screenshot', exact: true });
  await expect(image).toHaveAttribute('src', '/api/tracks/image-track/workspace/readfile-raw?path=screenshots%2Fpage.png');
  await expect.poll(() => image.evaluate((element: HTMLImageElement) => element.naturalWidth)).toBe(1200);
  await expect(page.getByRole('img', { name: 'Detail', exact: true })).toHaveAttribute('src',
    '/api/tracks/image-track/workspace/readfile-raw?path=screenshots%2Fdetail.png');
  await expect(page.getByText('Image unavailable: Missing', { exact: true })).toBeVisible();
  await expect(page.getByText('Image unavailable: Outside', { exact: true })).toBeVisible();
  const missingBox = await page.getByText('Image unavailable: Missing', { exact: true }).boundingBox();
  const outsideBox = await page.getByText('Image unavailable: Outside', { exact: true }).boundingBox();
  expect(outsideBox?.y).toBeGreaterThan(missingBox?.y ?? Infinity);
  expect(rawRequests.every((path) => path.startsWith('screenshots/'))).toBe(true);
  await page.screenshot({ path: testInfo.outputPath('planner-local-images-desktop.png') });
  await page.getByRole('button', { name: 'Close conversation' }).click();
  await page.getByRole('button', { name: /Conversation Planner/ }).click();
  await expect.poll(() => image.evaluate((element: HTMLImageElement) => element.naturalWidth)).toBe(1200);
  await page.reload();
  await expect.poll(() => image.evaluate((element: HTMLImageElement) => element.naturalWidth)).toBe(1200);
  await page.setViewportSize({ width: 390, height: 844 });
  await expect(image).toBeVisible();
  expect((await image.boundingBox())?.width).toBeLessThanOrEqual(390);
  await page.screenshot({ path: testInfo.outputPath('planner-local-images-mobile.png') });
});
