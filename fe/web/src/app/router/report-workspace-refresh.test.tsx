import { act, cleanup, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { queryKeys } from '../providers/queries.ts';
import { renderDailyFixture } from './daily-planner-fixture.tsx';

const fixtures: ReturnType<typeof renderDailyFixture>[] = [];
afterEach(() => {
  cleanup();
  for (const fixture of fixtures.splice(0)) fixture.client.clear();
  document.getElementById('root')?.remove();
});

function setup(initial = '/track/daily') {
  let revision = 1;
  let status = 'running';
  let text = '<!doctype html><h1>First result</h1>';
  const fixture = renderDailyFixture({ initial, reply: request => {
    const ok = (body: unknown) => ({ status: 200, statusText: 'OK', body });
    if (request.path === '/api/tracks/daily') return ok({ planner_revision: 0,
      track: { id: 'daily', area_id: 'daily-area', title: 'Portfolio', sort: 0, cwd: '/tmp', workspace: { isolation: 'managed', kind: 'managed', path: '/tmp', frozen_at: null },
        pinned_at: null, closed_at: null, created_at: 1, updated_at: 1 },
      can_reopen: false, can_admin: false, can_close: false, overlays: [], cards: [{
        id: 'report', track_id: 'daily', kind: 'track-report', title: null, sort: -1,
        deletable: false, created_at: 1, updated_at: revision,
        payload: { schemaVersion: 3, docRev: revision, summary: `Delivery ${revision}`, body: '', blocks: [
          { id: 'page', kind: 'html', rev: 1, payload: { path: 'index.html', title: 'Portfolio page' } },
        ] },
      }],
    });
    if (request.path === '/api/tracks/daily/report') return ok({ taskDiagnostics: [{
      blockId: 'task', key: 'edit', schedulable: true, status,
    }] });
    if (request.path === '/api/tracks/daily/workspace/readfile?path=index.html') return ok({
      path: 'index.html', size: text.length, text, truncated: false,
    });
    return undefined;
  } });
  fixtures.push(fixture);
  return { ...fixture, nextReport: () => { revision += 1; text = '<!doctype html><h1>Next result</h1>'; },
    completeTask: () => { status = 'completed'; text = '<!doctype html><h1>Completed result</h1>'; } };
}

for (const surface of ['report', 'file'] as const) {
  it(`reloads the mounted ${surface} page when a new report arrives without changing its HTML block`, async () => {
    const fixture = setup(surface === 'file' ? '/track/daily?file=index.html' : '/track/daily');
    const title = surface === 'file' ? 'index.html' : 'Portfolio page';
    const frame = () => [...document.querySelectorAll<HTMLIFrameElement>('iframe')]
      .find(candidate => candidate.title === title) ?? null;
    await waitFor(() => expect(frame()).not.toBeNull());
    const before = frame();
    const readsBefore = fixture.requests.filter(request => request.path.includes('/workspace/readfile?')).length;
    fixture.nextReport();
    await act(() => fixture.client.invalidateQueries({ queryKey: queryKeys.trackDetail('daily') }));
    await waitFor(() => {
      expect(frame()).not.toBeNull();
      expect(frame()).not.toBe(before);
    });
    expect(fixture.requests.filter(request => request.path.includes('/workspace/readfile?')).length).toBeGreaterThan(readsBefore);
    expect(frame()?.getAttribute('sandbox')).toBe('allow-scripts');
  });
}

it('reloads the mounted result after task completion even before the planner edits its report', async () => {
  const fixture = setup();
  const before = await screen.findByTitle('Portfolio page');
  fixture.completeTask();
  await act(() => fixture.client.invalidateQueries({ queryKey: queryKeys.trackReport('daily') }));
  await waitFor(() => expect(screen.getByTitle('Portfolio page')).not.toBe(before));
  expect(fixture.requests.filter(request => request.path.includes('/workspace/readfile?'))).toHaveLength(2);
});

it('keeps the live page mounted during an unchanged refetch', async () => {
  const fixture = setup();
  const before = await screen.findByTitle('Portfolio page');
  await act(() => fixture.client.invalidateQueries({ queryKey: queryKeys.trackDetail('daily') }));
  await act(() => fixture.client.invalidateQueries({ queryKey: queryKeys.trackReport('daily') }));
  expect(screen.getByTitle('Portfolio page')).toBe(before);
  expect(fixture.requests.filter(request => request.path.includes('/workspace/readfile?'))).toHaveLength(1);
});
