import { createMemoryHistory, createRootRoute, createRoute, createRouter, Outlet, RouterProvider } from '@tanstack/react-router';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { useGo, useRouteParam, useTrackFileNavigation, validateTrackSearch } from './navigation.ts';
import { TrackViewProvider, useTrackViewState } from './track-view-state.tsx';

function Probe({ trackId }: { trackId: string }) {
  useTrackViewState(trackId);
  const go = useGo();
  const file = useTrackFileNavigation();
  return <>
    <span>{trackId}</span>
    <button type="button" onClick={() => go({ name: 'track', trackId: 'a', from: 'pages' })}>resume a</button>
    <button type="button" onClick={() => go({ name: 'track', trackId: 'b' })}>resume b</button>
    <button type="button" onClick={() => go({ name: 'track', trackId: 'a', cardId: 'c' })}>target c</button>
    <button type="button" onClick={() => go({ name: 'track', trackId: 'a', blockId: 'paragraph' })}>target paragraph</button>
    <button type="button" onClick={() => file.closeFile(trackId)}>close file</button>
  </>;
}
function TrackProbe() {
  const id = useRouteParam('/track/')!;
  return <Probe key={id} trackId={id} />;
}
async function setup(entry: string) {
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
  const root = createRootRoute({ component: () => <TrackViewProvider><Outlet /></TrackViewProvider> });
  const track = createRoute({ getParentRoute: () => root, path: '/track/$trackId', validateSearch: validateTrackSearch, component: TrackProbe });
  const router = createRouter({ routeTree: root.addChildren([track]), history: createMemoryHistory({ initialEntries: [entry] }) });
  render(<RouterProvider router={router} />);
  await screen.findByText('a');
  return router;
}
async function click(label: string, trackId: string) {
  fireEvent.click(screen.getByRole('button', { name: label }));
  await waitFor(() => { expect(screen.getByText(trackId)).toBeTruthy(); });
}
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

it('resumes a mobile panel with the new return source and remembers an explicit close', async () => {
  const router = await setup('/track/a?panel=tasks&from=area');
  await click('resume b', 'b');
  expect(router.state.location.search).toEqual({});
  await click('resume a', 'a');
  expect(router.state.location.search).toEqual({ panel: 'tasks', from: 'pages' });
  await click('resume a', 'a');
  await waitFor(() => { expect(router.state.location.search).toEqual({ from: 'pages' }); });
  await click('resume b', 'b');
  await click('resume a', 'a');
  expect(router.state.location.search).toEqual({ from: 'pages' });
});

it('distinguishes a same-card explicit target from resume and clears the grid for anchors', async () => {
  const router = await setup('/track/a?card=c');
  await click('resume b', 'b');
  await click('target c', 'a');
  expect(router.state.location.search).toEqual({ card: 'c' });
  expect(router.state.location.state.ncResumeTrackView).toBe(false);
  await click('resume b', 'b');
  await click('resume a', 'a');
  expect(router.state.location.state.ncResumeTrackView).toBe(true);
  expect(router.state.location.search).toEqual({ card: 'c', from: 'pages' });
  await click('resume b', 'b');
  await click('target paragraph', 'a');
  expect(router.state.location.search).toEqual({});
  expect(router.state.location.hash).toBe('paragraph');
  expect(router.state.location.state.ncResumeTrackView).toBe(false);
});

it('closes a resumed file on its own track instead of popping to the previous track', async () => {
  const router = await setup('/track/a?file=README.md');
  await click('resume b', 'b');
  await click('resume a', 'a');
  expect(router.state.location.search).toEqual({ file: 'README.md', from: 'pages' });
  await click('close file', 'a');
  await waitFor(() => { expect(router.state.location.search).toEqual({ from: 'pages' }); });
  expect(router.state.location.pathname).toBe('/track/a');
});
