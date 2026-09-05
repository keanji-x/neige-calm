import { expect, test } from '@playwright/test';

// Browser integration with controlled HTTP responses; no worker is launched.
test('refreshing an exited terminal shows its final state while secondary reads are slow', async ({ page }, testInfo) => {
  const area = { id: 'area-1', name: 'Work', color: '#6a8', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
  const track = {
    id: 'track-1', area_id: area.id, title: 'Terminal lifecycle', sort: 1, lifecycle: 'working', cwd: '/tmp',
    archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 1,
  };
  let status = 'starting';
  let releaseSecondaryReads: () => void = () => {};
  const secondaryReads = new Promise<void>((resolve) => { releaseSecondaryReads = resolve; });
  const errors: string[] = [];
  page.on('pageerror', (error) => errors.push(error.message));
  await page.routeWebSocket('**/api/events**', () => {});
  await page.route(/^http:\/\/[^/]+\/api\//, async (route) => {
    const path = new URL(route.request().url()).pathname;
    let body: unknown;
    switch (path) {
      case '/api/auth/whoami':
        body = { userId: 'user-1', displayName: 'Test', role: 'admin', sessionId: 'session-1' };
        break;
      case '/api/version':
        body = { webCompatVersion: 23, minWebCompatVersion: 23, syncEventVersion: 1, dbInstanceId: '00000000-0000-4000-8000-000000000001' };
        break;
      case '/api/areas': body = [area]; break;
      case '/api/areas/area-1/tracks': body = [track]; break;
      case '/api/settings': body = {}; break;
      case '/api/tracks/track-1':
        body = { track, can_resume: false, overlays: [], cards: [{
          id: 'card-1', track_id: track.id, kind: 'terminal', title: 'Terminal', sort: 1,
          payload: {}, deletable: true, created_at: 1, updated_at: 1,
          runtime: { runtime_id: 'run-1', kind: 'terminal', status },
        }] };
        break;
      case '/api/tracks/track-1/report':
      case '/api/tracks/track-1/conversations':
      case '/api/tracks/track-1/backlinks':
        await secondaryReads;
        body = path.endsWith('/report') ? { taskDiagnostics: [] } : [];
        break;
      default: body = []; break;
    }
    await route.fulfill({ json: body });
  });
  try {
    await page.goto('/next/track/track-1?card=card-1');
    await expect(page.getByText('Starting terminal…')).toBeVisible();
    status = 'exited';
    await page.reload();
    await expect(page.getByText('Session exited.')).toBeVisible();
    await expect(page.getByText('Starting terminal…')).toHaveCount(0);
    await expect(page.getByRole('img', { name: 'status Working' })).toHaveCount(0);
    await page.screenshot({ path: testInfo.outputPath('terminal-exited.png'), fullPage: true });
    expect(errors).toEqual([]);
  } finally {
    releaseSecondaryReads();
  }
});
