import '../../styles/entry.css';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render } from '@testing-library/react';
import { page, userEvent, commands } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createAppRouter } from '../router/public.tsx';
import { bootTestCardRuntime } from '../router/test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';

declare module 'vitest/internal/browser' {
  interface BrowserCommands { emulateReducedMotion(reduce: boolean): Promise<void> }
}

afterEach(cleanup);
const AREA = { id: 'c1', name: 'Product', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
function setup(path: string, areaName = AREA.name, onRequest: (request: ApiRequest) => void = () => undefined, title = 'Responsive mobile UI', options: Readonly<{ trackAreaId?: string; failTracks?: boolean; failAreas?: boolean; longLists?: boolean }> = {}) {
  const track = { id: 'w1', area_id: options.trackAreaId ?? 'c1', title, sort: 1, lifecycle: 'working', cwd: '/tmp',
    archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2 };
  const report = { id: 'report', track_id: 'w1', title: 'Report', kind: 'track-report', sort: 0,
    deletable: false, created_at: 1, updated_at: 1, payload: { schemaVersion: 3, docRev: 1, summary: '', body: '',
      blocks: [{ id: 'prose', kind: 'prose', rev: 1, payload: { markdown: '## Findings\n\n[Source detail](neige://source/src_2c9e0a1b)' } }] } };
  const planner = { id: 'planner', track_id: 'w1', title: 'Design review', kind: 'codex', sort: 1,
    deletable: false, created_at: 1, updated_at: 1, payload: { planner_harness: true } };
  const source = { source_id: 'src_2c9e0a1b', provenance: 'manual', title: 'Source detail with a deliberately long title that must remain inside the shared mobile header',
    body: 'Existing source text.', body_bytes: 21, body_sha256: 'ab', captured_at: '2026-09-15T08:00:00Z',
    origin: { kind: 'manual' }, quotes: [] };
  const transport: ApiTransportPort = { send(request) {
    return Promise.resolve(reply(request));
  } };
  function reply(request: ApiRequest): ApiTransportResponse {
    onRequest(request);
    if (request.method === 'PATCH' && request.path === '/api/tracks/w1') {
      const next = (request.body as { title: string }).title;
      if (next === 'Rejected name') return { status: 500, statusText: 'Server Error', body: { error: 'Rename unavailable' } };
      track.title = next;
      return ok(track);
    }
    if ((options.failAreas && request.path === '/api/areas') || (options.failTracks && request.path === '/api/areas/c2/tracks')) return { status: 500, statusText: 'Server Error', body: { error: 'List unavailable' } };
    if (request.path === '/api/areas') return ok([...(options.longLists ? Array.from({ length: 18 }, (_, index) => ({ ...AREA, id: `extra${index}`, name: `Area ${index}`, sort: index - 18 })) : []), { ...AREA, name: areaName }, { ...AREA, id: 'c2', name: 'Frontend' }]);
    if (request.path === '/api/areas/c1/tracks') return ok([...(track.area_id === 'c1' ? [track] : []), { ...track, area_id: 'c1', id: 'w2', title: 'Another track' }, ...(options.longLists ? Array.from({ length: 25 }, (_, index) => ({ ...track, id: `long${index}`, title: `Long track ${index}` })) : [])]);
    if (request.path === '/api/areas/c2/tracks') return ok([{ ...track, area_id: 'c2', id: 'w3', title: 'Frontend track' }]);
    if (request.path === '/api/tracks/w1') return ok({ track, cards: [report, planner, { ...planner, id: 'terminal', title: 'Terminal', kind: 'terminal', payload: {} }], overlays: [], can_resume: false });
    if (request.path === '/api/tracks/w2') return ok({ track: { ...track, id: 'w2', title: 'Another track' }, cards: [], overlays: [], can_resume: false });
    if (request.path === '/api/tracks/w1/report') return ok({ taskDiagnostics: [] });
    if (request.path.endsWith('/sources/src_2c9e0a1b')) return ok(source);
    if (request.path.endsWith('/planner/run')) return ok({ card_id: 'planner', worker_session_id: 'runtime', phase: 'idle', model: null, reasoning_effort: null, blocked_reason: null });
    if (request.path === '/api/settings') return ok({ settings: {} });
    return ok([]);
  }
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ transport, unauthorized: createUnauthorizedChannel({ enqueue: (task) => task() }),
    client, cards: bootTestCardRuntime(), onSignOut: vi.fn() });
  router.update({ history: createMemoryHistory({ initialEntries: [path] }) });
  render(<QueryClientProvider client={client}><ThemeProvider storage={{ getItem: () => null, setItem: () => undefined }}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return router;
}
const settlePaint = () => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));

describe.each([390, 1280])('Settings resizing from %ipx', (initialWidth) => {
it.each([
  { section: 'network', label: 'HTTP proxy', key: 'http_proxy', value: 'http://draft-proxy:3128' },
  { section: 'network', label: 'HTTPS proxy', key: 'https_proxy', value: 'http://draft-secure-proxy:3128' },
  { section: 'general', label: 'Task concurrency', key: 'task_budget_default', value: '7' },
] as const)('preserves an unfinished $label across Settings resize until a real blur', async ({ section, label, key, value }) => {
  await page.viewport(initialWidth, 844);
  const writes: ApiRequest[] = [];
  setup(`/settings/${section}`, AREA.name, (request) => { if (request.method !== 'GET') writes.push(request); });
  const field = page.getByRole(section === 'general' ? 'spinbutton' : 'textbox', { name: label, exact: true });
  await field.fill(value);
  const input = await field.findElement() as HTMLInputElement;
  if (input.selectionStart !== null) input.setSelectionRange(2, 5);
  const selection = [input.selectionStart, input.selectionEnd];
  for (const width of [1280, 390, 1280, 390]) {
    await page.viewport(width, 900);
    await settlePaint();
    expect(writes).toEqual([]);
    expect(await field.findElement()).toBe(input);
    expect(input.value).toBe(value);
    expect(document.activeElement).toBe(input);
    expect([input.selectionStart, input.selectionEnd]).toEqual(selection);
  }
  await userEvent.keyboard('{Tab}');
  await expect.poll(() => writes.length).toBe(1);
  expect(writes[0].body).toEqual({ settings: { [key]: value } });
  await page.viewport(1280, 720);
});
});

it('removes abandoned title containers when switching Tracks', async () => {
  await page.viewport(390, 844);
  const router = setup('/track/w1');
  const selector = await page.getByRole('button', { name: 'Switch track, Responsive mobile UI', exact: true }).findElement();
  const host = selector.closest('[style="display: contents;"]')!.parentElement!;
  for (const id of ['w2', 'w1', 'w2', 'w1']) {
    await router.navigate({ to: '/track/$trackId', params: { trackId: id } });
    await page.getByRole('button', { name: `Switch track, ${id === 'w1' ? 'Responsive mobile UI' : 'Another track'}`, exact: true }).findElement();
    await settlePaint();
    expect(host.childElementCount).toBe(1);
    expect(host.firstElementChild?.childElementCount).toBeGreaterThan(0);
  }
  await page.viewport(1280, 720);
});

it('uses Escape to close Planner before its Conversations page', async () => {
  await page.viewport(390, 844);
  const router = setup('/track/w1');
  await page.getByRole('button', { name: 'Track actions', exact: true }).click();
  await page.getByRole('menuitem', { name: 'Conversations', exact: true }).click();
  await page.getByRole('button', { name: /Design review/ }).click();
  await page.getByRole('heading', { name: 'Design review', exact: true }).findElement();
  await userEvent.keyboard('{Escape}');
  await settlePaint();
  expect(router.state.location.search.panel).toBe('conversations');
  await expect.element(page.getByRole('heading', { name: 'Conversations', exact: true })).toBeVisible();
  expect(document.querySelector('[data-nc-drawer]')).toBeNull();
  await expect.poll(() => document.activeElement).toBe(await page.getByRole('button', { name: /Design review/ }).findElement());
  await userEvent.keyboard('{Escape}');
  await settlePaint();
  expect(router.state.location.search.panel).toBeUndefined();
  await page.viewport(1280, 720);
});

async function editTrack(): Promise<void> {
  // Also cover keyboard reopening; pointer close/reopen has Astryx's short
  // light-dismiss click fence, exercised by the initial pointer entry above.
  const trigger = await page.getByRole('button', { name: 'Track actions', exact: true }).findElement();
  (trigger as HTMLElement).focus();
  await userEvent.keyboard('{ArrowDown}');
  await page.getByRole('menuitem', { name: 'Edit track', exact: true }).click();
}

describe('Unified mobile headers', () => {
  it('uses clean Area rows and a separate Tracks page with the same actions', async () => {
    await page.viewport(390, 844);
    const router = setup('/track/w1');
    await page.getByRole('button', { name: 'Open areas' }).click();
    const area = await page.getByRole('button', { name: 'Product', exact: true }).findElement();
    expect(area.closest('li')?.textContent).toBe('Product');
    const row = area.closest('li')!;
    const icons = row.querySelectorAll('svg');
    expect(icons).toHaveLength(2);
    expect(icons[1].getBoundingClientRect().right).toBeGreaterThan(row.getBoundingClientRect().right - 32);
    expect(row.querySelector('[style]')).toBeNull();
    await settlePaint();
    await Promise.all(document.getAnimations().filter((animation) => animation.effect?.getTiming().iterations !== Infinity).map((animation) => animation.finished.catch(() => undefined)));
    await page.screenshot({ path: '../../../../test-results/clean-390-areas.png' });
    expect(page.getByRole('button', { name: 'Responsive mobile UI', exact: true }).query()).toBeNull();
    await page.elementLocator(area).click();
    await page.getByRole('heading', { name: 'Product', exact: true }).findElement();
    await settlePaint();
    await Promise.all(document.getAnimations().filter((animation) => animation.effect?.getTiming().iterations !== Infinity).map((animation) => animation.finished.catch(() => undefined)));
    await page.screenshot({ path: '../../../../test-results/clean-390-tracks.png' });
    await page.getByRole('button', { name: 'Responsive mobile UI', exact: true }).findElement();
    expect(document.querySelector('[data-nc-workspace-page="tracks"] [aria-label="Workspace actions"]')?.textContent).toBe('SettingsNew track');
    expect(router.state.location.pathname).toBe('/track/w1');
    await page.getByRole('button', { name: 'Back to Areas' }).click();
    await page.getByRole('heading', { name: 'Areas', exact: true }).findElement();
    await page.getByRole('button', { name: 'Back to workspace' }).click();
    expect(router.state.location.pathname).toBe('/track/w1');
    await page.viewport(1280, 720);
  });

  it('switches complete Area pages directly and gives Tracks matching file icons', async () => {
    await page.viewport(390, 844);
    const router = setup('/track/w1');
    await page.getByRole('button', { name: 'Open areas' }).click();
    const areas = document.querySelector<HTMLElement>('[data-nc-workspace-page="areas"]')!;
    const tracks = document.querySelector<HTMLElement>('[data-nc-workspace-page="tracks"]')!;
    const folder = (await page.getByRole('button', { name: 'Product', exact: true }).findElement()).closest('li')!.querySelector('svg')!;
    const folderSize = folder.getBoundingClientRect().width;
    const folderColor = getComputedStyle(folder).color;
    await page.getByRole('button', { name: 'Product', exact: true }).click();
    expect(tracks.getAnimations()).toHaveLength(0);
    expect(tracks.getBoundingClientRect().left).toBe(0);
    expect(tracks.getBoundingClientRect().width).toBe(390);
    expect(tracks.getBoundingClientRect().height).toBe(844);
    expect(areas.isConnected).toBe(true); expect(areas.inert).toBe(true);
    const row = await page.getByRole('button', { name: 'Responsive mobile UI', exact: true }).findElement();
    const file = row.querySelector('svg'); expect(file).not.toBeNull();
    expect(file!.getBoundingClientRect().width).toBe(folderSize);
    expect(getComputedStyle(file!).color).toBe(folderColor);
    await page.screenshot({ path: '../../../../test-results/direct-track-file-list.png' });
    const back = await page.getByRole('button', { name: 'Back to Areas' }).findElement();
    (back as HTMLElement).focus();
    await userEvent.keyboard('{Shift>}{Tab}{/Shift}');
    expect(document.activeElement).toBe(await page.getByRole('button', { name: 'Another track', exact: true }).findElement());
    await userEvent.keyboard('{Tab}'); expect(document.activeElement).toBe(back);
    await page.elementLocator(back).click();
    expect(areas.getAnimations()).toHaveLength(0);
    expect(areas.getBoundingClientRect().left).toBe(0);
    expect(tracks.inert).toBe(true);
    expect(router.state.location.pathname).toBe('/track/w1');
    await page.viewport(1280, 720);
  });

  it('keeps Areas scroll on return, starts Tracks at the top, and handles quick back/forward', async () => {
    await page.viewport(320, 520);
    const router = setup('/track/w1', AREA.name, () => undefined, 'Responsive mobile UI', { longLists: true });
    await page.getByRole('button', { name: 'Open areas' }).click();
    const areas = document.querySelector<HTMLElement>('[data-nc-workspace-page="areas"]')!;
    const tracks = document.querySelector<HTMLElement>('[data-nc-workspace-page="tracks"]')!;
    areas.scrollTop = areas.scrollHeight;
    await settlePaint();
    const position = areas.scrollTop; expect(position).toBeGreaterThan(0);
    await page.getByRole('button', { name: 'Product', exact: true }).click();
    await Promise.all(tracks.getAnimations().map((motion) => motion.finished));
    expect(tracks.scrollTop).toBe(0);
    tracks.scrollTop = 500; await settlePaint(); expect(tracks.scrollTop).toBe(500);
    await page.getByRole('button', { name: 'Back to Areas' }).click();
    expect(areas.scrollTop).toBe(position);
    await page.getByRole('button', { name: 'Product', exact: true }).click();
    await Promise.all(tracks.getAnimations().map((motion) => motion.finished));
    expect(tracks.scrollTop).toBe(0);
    expect((await page.getByRole('heading', { name: 'Product', exact: true }).findElement()).closest('header')!.getBoundingClientRect().top).toBe(0);
    expect(areas.scrollTop).toBe(position);
    expect(router.state.location.pathname).toBe('/track/w1');
    await page.screenshot({ path: '../../../../test-results/area-page-short-screen.png' });
    await page.viewport(1280, 720);
  });

  it('switches whole pages without animation when reduced motion is requested', async () => {
    await commands.emulateReducedMotion(true);
    try {
      await page.viewport(390, 844);
      setup('/track/w1');
      await page.getByRole('button', { name: 'Open areas' }).click();
      await page.getByRole('button', { name: 'Product', exact: true }).click();
      const tracks = document.querySelector<HTMLElement>('[data-nc-workspace-page="tracks"]')!;
      expect(tracks.getAnimations()).toHaveLength(0);
      expect(tracks.getBoundingClientRect().left).toBe(0);
      await page.getByRole('button', { name: 'Back to Areas' }).click();
      expect(document.querySelector<HTMLElement>('[data-nc-workspace-page="areas"]')!.getBoundingClientRect().left).toBe(0);
    } finally {
      await commands.emulateReducedMotion(false);
      await page.viewport(1280, 720);
    }
  });

  it('reserves the mobile title for switching and opens rename from More', async () => {
    await page.viewport(390, 844);
    setup('/track/w1');
    await page.getByRole('button', { name: 'Switch track, Responsive mobile UI', exact: true }).findElement();
    expect(page.getByRole('button', { name: 'Edit track', exact: true }).query()).toBeNull();
    await settlePaint();
    await Promise.all(document.getAnimations().filter((animation) => animation.effect?.getTiming().iterations !== Infinity).map((animation) => animation.finished.catch(() => undefined)));
    await page.screenshot({ path: '../../../../test-results/clean-390-track.png' });
    await page.getByRole('button', { name: 'Track actions', exact: true }).click();
    const menu = await page.getByRole('menu').findElement();
    const order = [...menu.querySelectorAll('[role="menuitem"], [role="separator"]')]
      .map((item) => item.getAttribute('role') === 'separator' ? 'separator' : item.textContent?.trim());
    const editIndex = order.indexOf('Edit track');
    expect(editIndex).toBeGreaterThan(0);
    expect(order[editIndex - 1]).toBe('separator');
    expect(order[editIndex + 1]).toBe('Delete track');
    await settlePaint();
    await page.screenshot({ path: '../../../../test-results/track-more-management-group.png' });
    await page.getByRole('menuitem', { name: 'Edit track', exact: true }).click();
    const input = await page.getByRole('textbox', { name: 'Track title', exact: true }).findElement();
    expect(document.activeElement).toBe(input);
    await settlePaint();
    await Promise.all(document.getAnimations().filter((animation) => animation.effect?.getTiming().iterations !== Infinity).map((animation) => animation.finished.catch(() => undefined)));
    await page.screenshot({ path: '../../../../test-results/clean-390-edit-track.png' });
    await userEvent.keyboard('{Escape}');
    await page.viewport(1280, 720);
  });

  it('switches Tracks from the title and reserves renaming for the independent Edit button', async () => {
    await page.viewport(390, 844);
    const writes: ApiRequest[] = [];
    const router = setup('/track/w1', AREA.name, (request) => { if (request.method !== 'GET') writes.push(request); });
    await page.getByRole('button', { name: 'Track actions', exact: true }).findElement();
    const selector = page.getByRole('button', { name: 'Switch track, Responsive mobile UI', exact: true });
    expect(selector.query()).not.toBeNull();
    await selector.click();
    expect(page.getByRole('textbox', { name: 'Track title', exact: true }).query()).toBeNull();
    expect((await page.getByRole('menuitem', { name: 'Responsive mobile UI', exact: true }).findElement()).getAttribute('aria-current')).toBe('page');
    await page.getByRole('menuitem', { name: 'Another track', exact: true }).click();
    await expect.poll(() => router.state.location.pathname).toBe('/track/w2');
    expect(writes).toEqual([]);
    await editTrack();
    const input = page.getByRole('textbox', { name: 'Track title', exact: true });
    await input.fill('Cancelled draft');
    await userEvent.keyboard('{Escape}');
    await page.getByRole('button', { name: 'Switch track, Another track', exact: true }).findElement();
    expect(writes).toEqual([]);
    await page.viewport(1280, 720);
  });

  it.each(['tracks', 'areas'])('uses the Track’s exact Area and exposes %s read failure with a dismissible menu', async (failure) => {
    await page.viewport(320, 844);
    setup('/track/w1', AREA.name, () => undefined, 'Second Area track', { trackAreaId: 'c2', failTracks: failure === 'tracks', failAreas: failure === 'areas' });
    const selector = page.getByRole('button', { name: 'Switch track, Second Area track', exact: true });
    await selector.click();
    await page.getByRole('menuitem', { name: /Could not read tracks/ }).findElement();
    expect(page.getByRole('menuitem', { name: 'Another track', exact: true }).query()).toBeNull();
    await page.getByRole('menuitem', { name: 'Retry', exact: true }).findElement();
    await userEvent.keyboard('{Escape}');
    expect(page.getByRole('menu').query()).toBeNull();
    expect(document.activeElement).toBe(await selector.findElement());
    await page.viewport(1280, 720);
  });

  it('keeps both navigation levels local and returns to the original content', async () => {
    await page.viewport(390, 844);
    const router = setup('/track/w1');
    const opener = await page.getByRole('button', { name: 'Open areas' }).findElement();
    await page.elementLocator(opener).click();
    const frontend = page.getByRole('button', { name: 'Frontend', exact: true });
    const back = await page.getByRole('button', { name: 'Back to workspace' }).findElement();
    (back as HTMLElement).focus();
    await userEvent.keyboard('{Shift>}{Tab}{/Shift}');
    expect(document.activeElement).toBe(await frontend.findElement());
    await userEvent.keyboard('{Tab}');
    expect(document.activeElement).toBe(back);
    await frontend.click();
    await page.getByRole('heading', { name: 'Frontend', exact: true }).findElement();
    expect(document.activeElement).not.toBe(document.body);
    expect(router.state.location.pathname).toBe('/track/w1');
    await page.getByRole('button', { name: 'Back to Areas' }).click();
    await page.getByRole('heading', { name: 'Areas', exact: true }).findElement();
    expect(document.activeElement).not.toBe(document.body);
    await page.getByRole('button', { name: 'Back to workspace' }).click();
    expect(document.activeElement).toBe(opener);
    expect(router.state.location.pathname).toBe('/track/w1');
    await page.viewport(1280, 720);
  });

  it('keeps the live rename input and draft without a PATCH when changing header hosts', async () => {
    await page.viewport(390, 844);
    const writes: ApiRequest[] = [];
    setup('/track/w1', AREA.name, (request) => { if (request.method !== 'GET') writes.push(request); });
    await editTrack();
    const input = await page.getByRole('textbox', { name: 'Track title', exact: true }).findElement();
    await page.getByRole('textbox', { name: 'Track title', exact: true }).fill('Rename draft before resize');
    (input as HTMLInputElement).setSelectionRange(2, 8);
    await page.viewport(1280, 900);
    const desktopInput = await page.getByRole('textbox', { name: 'Track title', exact: true }).findElement();
    expect(desktopInput).toBe(input);
    expect(document.activeElement).toBe(input);
    expect((input as HTMLInputElement).value).toBe('Rename draft before resize');
    expect([(input as HTMLInputElement).selectionStart, (input as HTMLInputElement).selectionEnd]).toEqual([2, 8]);
    expect(writes).toEqual([]);
    await page.viewport(390, 844);
    const phoneInput = await page.getByRole('textbox', { name: 'Track title', exact: true }).findElement();
    expect(phoneInput).toBe(input);
    expect(document.activeElement).toBe(input);
    expect((input as HTMLInputElement).value).toBe('Rename draft before resize');
    expect([(input as HTMLInputElement).selectionStart, (input as HTMLInputElement).selectionEnd]).toEqual([2, 8]);
    expect(writes).toEqual([]);
    await userEvent.keyboard('{Escape}');
    expect(writes).toEqual([]);
    await page.getByRole('button', { name: /^Switch track,/ }).findElement();
  });

  it('does not open the title menu or editor from the synthesized click after an Enter commit', async () => {
    await page.viewport(390, 844);
    const writes: ApiRequest[] = [];
    setup('/track/w1', AREA.name, (request) => { if (request.method === 'PATCH') writes.push(request); });
    await editTrack();
    const input = page.getByRole('textbox', { name: 'Track title', exact: true });
    await input.fill('Committed title');
    await userEvent.keyboard('{Enter}');
    const selector = await page.getByRole('button', { name: 'Switch track, Committed title', exact: true }).findElement();
    selector.dispatchEvent(new MouseEvent('click', { bubbles: true, detail: 0 }));
    await settlePaint();
    expect(page.getByRole('textbox', { name: 'Track title', exact: true }).query()).toBeNull();
    expect(page.getByRole('menu').query()).toBeNull();
    expect(writes).toHaveLength(1);
    await page.viewport(1280, 720);
  });

  it('still commits a real user blur and preserves empty names and rejected rename drafts', async () => {
    await page.viewport(390, 844);
    const writes: ApiRequest[] = [];
    setup('/track/w1', AREA.name, (request) => { if (request.method === 'PATCH') writes.push(request); }, '');
    await editTrack();
    const input = page.getByRole('textbox', { name: 'Track title', exact: true });
    expect((await input.findElement() as HTMLInputElement).value).toBe('');
    await userEvent.keyboard('{Escape}');
    expect(writes).toEqual([]);
    await editTrack();
    await input.fill('Changed by user');
    await page.getByRole('button', { name: 'Track actions', exact: true }).click();
    await expect.poll(() => writes.length).toBe(1);
    expect(writes[0].body).toEqual({ title: 'Changed by user' });
    await userEvent.keyboard('{Escape}');
    await editTrack();
    await input.fill('Rejected name');
    await page.getByRole('button', { name: 'Track actions', exact: true }).click();
    await expect.poll(() => writes.length).toBe(2);
    const failedInput = await input.findElement();
    expect((failedInput as HTMLInputElement).value).toBe('Rejected name');
    expect(page.getByRole('menuitem', { name: 'Edit track', exact: true }).query()).toBeNull();
    await userEvent.keyboard('{Escape}');
    expect(await input.findElement()).toBe(failedInput);
    await page.elementLocator(failedInput).click();
    expect(document.activeElement).toBe(failedInput);
    await userEvent.keyboard('{Escape}');
    await editTrack();
    expect((await input.findElement() as HTMLInputElement).value).toBe('Changed by user');
    await userEvent.keyboard('{Escape}');
  });

  it.each([320, 390])('measures the same header frame across the composed mobile pages at %ipx', async (width) => {
    await page.viewport(width, 844);
    const router = setup('/track/w1');
    const frames: Array<{ name: string; x: number; y: number; width: number; height: number; left: number; top: number }> = [];
    const record = async (name: string, header: Element) => {
      await Promise.all(header.closest('[data-nc-mobile-page], [data-nc-drawer], [data-nc-workspace-page]')?.getAnimations().map((animation) => animation.finished) ?? []);
      await Promise.all(header.getAnimations({ subtree: true }).filter((animation) => animation.effect?.getTiming().iterations !== Infinity)
        .map((animation) => animation.finished.catch(() => undefined)));
      await settlePaint();
      const box = header.getBoundingClientRect();
      const button = header.querySelector('button')!;
      const hit = button.getBoundingClientRect();
      frames.push({ name, x: box.x, y: box.y, width: box.width, height: box.height, left: hit.x, top: hit.y });
      expect(box.x).toBe(0); expect(box.y).toBe(0); expect(box.width).toBe(width); expect(box.height).toBe(56);
      expect(hit.x).toBe(16); expect(hit.y).toBe(6); expect(hit.width).toBe(44); expect(hit.height).toBe(44);
      const title = header.querySelector<HTMLElement>('[aria-label^="Switch track,"]')
        ?? header.querySelector<HTMLElement>('[aria-label^="Switch area,"]')
        ?? header.querySelector<HTMLElement>('h1, h2')!;
      expect(getComputedStyle(title).fontSize).toBe('16px');
      expect(getComputedStyle(title).fontWeight).toBe('500');
      expect(title.getBoundingClientRect().right).toBeLessThanOrEqual(width - 60);
      const right = header.querySelector<HTMLElement>(':scope > :last-child button:not([role="menuitem"])');
      if (right !== null) {
        const rightBox = right.getBoundingClientRect();
        expect(rightBox.x).toBe(width - 60); expect(rightBox.y).toBe(6);
        expect(rightBox.width).toBe(44); expect(rightBox.height).toBe(44);
        const glyph = right.querySelector('svg');
        if (glyph !== null) {
          expect(glyph.getBoundingClientRect().width).toBe(20); expect(glyph.getBoundingClientRect().height).toBe(20);
          expect(getComputedStyle(glyph).visibility).toBe('visible');
          let effectiveOpacity = 1;
          for (let node: Element | null = glyph; node !== null && node !== header; node = node.parentElement) effectiveOpacity *= Number(getComputedStyle(node).opacity);
          expect(effectiveOpacity).toBe(1);
        }
      }


      await page.screenshot({ path: `../../../../test-results/unified-${width}-${name}.png` });
    };
    await page.getByRole('button', { name: /^Switch track,/ }).findElement();
    await record('track', document.querySelector('[data-nc-workspace-header] header')!);
    const selector = await page.getByRole('button', { name: 'Switch track, Responsive mobile UI', exact: true }).findElement();
    const more = await page.getByRole('button', { name: 'Track actions', exact: true }).findElement();
    expect(selector.getBoundingClientRect().right).toBeLessThanOrEqual(more.getBoundingClientRect().left);
    expect(Math.abs(selector.getBoundingClientRect().left + selector.getBoundingClientRect().width / 2 - width / 2)).toBeLessThan(1);
    await page.elementLocator(selector).click();
    const trackMenu = await page.getByRole('menu').findElement();
    const choice = await page.getByRole('menuitem', { name: 'Responsive mobile UI', exact: true }).findElement();
    const file = choice.querySelector('svg'); expect(file).not.toBeNull();
    expect(file!.getBoundingClientRect().width).toBe(20);
    expect(trackMenu.getBoundingClientRect().left).toBeGreaterThanOrEqual(0);
    expect(trackMenu.getBoundingClientRect().right).toBeLessThanOrEqual(width);
    await page.screenshot({ path: `../../../../test-results/grouped-${width}-track-menu.png` });
    await userEvent.keyboard('{Escape}');
    expect(document.activeElement).toBe(selector);
    await page.getByRole('button', { name: 'Open areas' }).click();
    await record('areas', (await page.getByRole('heading', { name: 'Areas', exact: true }).findElement()).closest('header')!);
    await page.getByRole('button', { name: 'Product', exact: true }).click();
    await record('tracks', (await page.getByRole('heading', { name: 'Product', exact: true }).findElement()).closest('header')!);
    await page.getByRole('button', { name: 'New track', exact: true }).click();
    await record('new-track', (await page.getByRole('button', { name: 'Switch area, Product' }).findElement()).closest('header')!);
    await router.navigate({ to: '/track/w1' });
    await page.getByRole('button', { name: 'Track actions', exact: true }).click();
    await page.getByRole('menuitem', { name: 'Cards', exact: true }).click();
    await record('cards', (await page.getByRole('heading', { name: 'Cards', exact: true }).findElement()).closest('header')!);
    await page.getByRole('button', { name: 'Back to Report' }).click();
    await page.getByRole('button', { name: 'Track actions', exact: true }).click();
    await page.getByRole('menuitem', { name: 'Conversations', exact: true }).click();
    await record('conversations', (await page.getByRole('heading', { name: 'Conversations', exact: true }).findElement()).closest('header')!);
    await page.getByRole('button', { name: /Conversation Design review/ }).click();
    await record('chat', (await page.getByRole('heading', { name: 'Design review', exact: true }).findElement()).closest('header')!);
    await page.getByRole('button', { name: 'Back to Conversations' }).click();
    await expect.poll(() => document.querySelector('[data-nc-drawer]')).toBeNull();
    await page.getByRole('button', { name: 'Back to Report' }).click();
    await page.getByRole('button', { name: 'Source detail', exact: true }).click();
    await page.getByText('Existing source text.', { exact: true }).findElement();
    await record('source', document.querySelector('[data-nc-drawer] header')!);
    await page.getByRole('button', { name: 'Back to Report' }).click();
    await expect.poll(() => document.querySelector('[data-nc-drawer]')).toBeNull();
    await page.getByRole('button', { name: 'Open areas' }).click();
    await page.getByRole('button', { name: 'Settings', exact: true }).click();
    await record('settings', (await page.getByRole('heading', { name: 'Settings', exact: true }).findElement()).closest('header')!);
    expect(frames).toHaveLength(9);
  });

  it('uses one shared header on a cold card view instead of stacking the shell and card headers', async () => {
    await page.viewport(390, 844);
    setup('/track/w1?card=terminal');
    const back = await page.getByRole('button', { name: 'Back to Report', exact: true }).findElement();
    const header = back.closest('header')!;
    expect(header.getBoundingClientRect().y).toBe(0);
    const painted = [...document.querySelectorAll('[data-nc-mobile-header]')]
      .filter((element) => element.getBoundingClientRect().height > 0);
    expect(painted).toHaveLength(1);
  });

});


it.each(['Switch track, Responsive mobile UI'])('hands read-mode %s focus to the desktop Rename control when resizing', async (name) => {
  await page.viewport(390, 844);
  setup('/track/w1');
  const current = await page.getByRole('button', { name, exact: true }).findElement();
  (current as HTMLElement).focus();
  expect(document.activeElement).toBe(current);
  await page.viewport(1280, 900);
  const desktop = await page.getByRole('button', { name: 'Rename track', exact: true }).findElement();
  await settlePaint();
  expect(document.activeElement).toBe(desktop);
});

it('hands desktop Rename focus to the mobile Track selector when resizing', async () => {
  await page.viewport(1280, 900);
  setup('/track/w1');
  const desktop = await page.getByRole('button', { name: 'Rename track', exact: true }).findElement();
  (desktop as HTMLElement).focus();
  await page.viewport(390, 844);
  const edit = await page.getByRole('button', { name: /^Switch track,/ }).findElement();
  await settlePaint();
  expect(document.activeElement).toBe(edit);
  await page.viewport(1280, 720);
});

it.each(['outside', 'blur'])('does not reclaim title focus after it has moved %s before resizing', async (destination) => {
  await page.viewport(390, 844);
  setup('/track/w1');
  const title = await page.getByRole('button', { name: 'Switch track, Responsive mobile UI', exact: true }).findElement() as HTMLElement;
  title.focus();
  const source = await page.getByRole('button', { name: 'Source detail', exact: true }).findElement() as HTMLElement;
  if (destination === 'outside') source.focus(); else title.blur();
  await page.viewport(1280, 900);
  await page.getByRole('button', { name: 'Rename track', exact: true }).findElement();
  await settlePaint();
  expect(document.activeElement).toBe(destination === 'outside' ? source : document.body);
  await page.viewport(390, 844);
  await page.getByRole('button', { name: /^Switch track,/ }).findElement();
  await settlePaint();
  expect(document.activeElement).toBe(destination === 'outside' ? source : document.body);
  await page.viewport(1280, 720);
});
