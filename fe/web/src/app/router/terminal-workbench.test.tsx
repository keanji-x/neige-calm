import { act, cleanup, fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import type { Conversation } from '../../../../core/domain/conversation.ts';
import { queryKeys } from '../providers/queries.ts';
import { setupWorkbench, terminalCard } from './terminal-workbench-fixture.tsx';

afterEach(() => { cleanup(); document.getElementById('root')?.remove(); vi.restoreAllMocks(); });

it.each(['main', 'draft', 'side'] as const)('dispatches /terminal from the %s pane to the latest committed card list and pending intent', async pane => {
  const fixture = setupWorkbench({ defer: true, failure: 'definite' });
  await userEvent.click(await screen.findByRole('button', { name: pane === 'draft' ? /^New conversation$/u : 'Conversation Main discussion' }));
  const command = async (input: HTMLElement, text: string) => {
    await waitFor(() => expect(input.getAttribute('contenteditable')).toBe('true'));
    input.textContent = text; fireEvent.input(input);
    fireEvent.keyDown(input, { key: 'Enter', code: 'Enter' });
  };
  if (pane === 'side') {
    await waitFor(() => expect(document.querySelector('[data-nc-thread]')).not.toBeNull());
    await command(await screen.findByRole('combobox', { name: 'Message' }), '/side');
    await screen.findByRole('region', { name: 'Side conversation · Codex' });
  }
  const input = pane === 'side'
    ? within(screen.getByRole('region', { name: 'Side conversation · Codex' })).getByRole('combobox', { name: 'Message' })
    : await screen.findByRole('combobox', { name: 'Message' });
  if (pane === 'main') {
    fixture.live.text = 'Streamed before terminal intent';
    await act(async () => { await fixture.client.refetchQueries({ queryKey: queryKeys.plannerRun('parent') }); });
    await act(async () => { await fixture.client.refetchQueries({ queryKey: queryKeys.harnessLive('parent') }); });
    await screen.findByText('Streamed before terminal intent');
  }
  // The composer was already committed with an empty card list. A new list
  // must change the event implementation without freezing that initial intent.
  fixture.cards.push(terminalCard('arrived'));
  await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.trackDetail('workbench') }); });
  // Query invalidation resolves before its observer notification commits.
  // Wait for the real inventory projection, independently of composer typing.
  await screen.findByRole('button', { name: /^arrived/ });
  await command(input, '/terminal');
  await screen.findByRole('button', { name: 'Hide terminal' });
  expect(document.querySelector('[data-nc-card-dock]:not([hidden])')?.getAttribute('data-nc-card-id')).toBe('arrived');
  expect(fixture.posts()).toHaveLength(0);
  await userEvent.click(screen.getByRole('button', { name: 'Hide terminal' }));
  fixture.cards.length = 0;
  await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.trackDetail('workbench') }); });
  await waitFor(() => expect(screen.queryByRole('button', { name: /^arrived/ })).toBeNull());
  await command(input, '/terminal'); await command(input, '/terminal');
  await waitFor(() => expect(fixture.posts()).toHaveLength(1));
  await act(async () => { fixture.releases[0]?.(); await Promise.resolve(); });
  await screen.findByText('Terminal refused');
  await command(input, '/terminal');
  await waitFor(() => expect(fixture.posts()).toHaveLength(2));
  expect(fixture.posts()[1]?.headers?.['Idempotency-Key']).not.toBe(fixture.posts()[0]?.headers?.['Idempotency-Key']);
  await act(async () => { fixture.releases[1]?.(); await Promise.resolve(); });
  await screen.findByText('No terminal session available.');
});

it('opens the fixed entry without a conversation, deduplicates pending presses and retains the host when hidden', async () => {
  const fixture = setupWorkbench({ defer: true });
  const mount = vi.spyOn(fixture.runtime.host, 'mount');
  const entry = await screen.findByRole('button', { name: 'Terminal workbench' });
  expect(entry.closest('section')?.getAttribute('aria-label')).toBe('Workbench');
  expect(screen.getByRole('region', { name: 'Workbench' }).textContent).toContain('No tools yet.');
  expect(screen.getByRole('region', { name: 'Conversations' }).contains(entry)).toBe(false);
  await userEvent.click(entry); await userEvent.click(entry);
  await waitFor(() => expect(fixture.posts()).toHaveLength(1));
  await act(async () => { fixture.releases[0]?.(); await Promise.resolve(); });
  await screen.findByText('No terminal session available.');
  expect(mount).toHaveBeenCalledTimes(1);
  const hide = screen.getByRole('button', { name: 'Hide terminal' });
  expect(hide.textContent).toBe('');
  expect(hide.querySelector('svg')?.classList.contains('lucide-chevron-right')).toBe(true);
  const dock = document.querySelector('[data-nc-card-dock]');
  const resource = document.querySelector('[data-nc-terminal-card]');
  await userEvent.click(screen.getByRole('button', { name: 'Hide terminal' }));
  expect(document.querySelector('[data-nc-card-dock]')).toBe(dock);
  expect(document.querySelector('[data-nc-terminal-card]')).toBe(resource);
  expect(mount).toHaveBeenCalledTimes(1);
  await userEvent.click(entry);
  expect(document.querySelector('[data-nc-card-dock]')).toBe(dock);
  expect(document.querySelector('[data-nc-terminal-card]')).toBe(resource);
  expect(mount).toHaveBeenCalledTimes(1);
  expect(fixture.posts()).toHaveLength(1);
  expect(fixture.requests.some(request => request.method === 'DELETE')).toBe(false);
});

it('retries an unknown response with the original key and first body despite a theme change', async () => {
  const fixture = setupWorkbench({ lost: true });
  await userEvent.click(await screen.findByRole('button', { name: 'Terminal workbench' }));
  await screen.findByRole('alert');
  document.documentElement.dataset.theme = 'light';
  await userEvent.click(screen.getByRole('button', { name: 'Try again' }));
  await screen.findByText('No terminal session available.');
  const posts = fixture.posts();
  expect(posts).toHaveLength(2);
  expect(posts[0]?.headers?.['Idempotency-Key']).toBeTruthy();
  expect(posts[1]?.headers).toEqual(posts[0]?.headers);
  expect(posts[1]?.body).toEqual(posts[0]?.body);
  expect(posts[0]?.body).not.toHaveProperty('cwd');
});

it('shares /terminal with the fixed entry in an empty draft and refuses arguments without sending', async () => {
  const fixture = setupWorkbench({ cards: [terminalCard('existing')] });
  await userEvent.click(await screen.findByRole('button', { name: /^New conversation$/u }));
  const input = await screen.findByRole('combobox', { name: 'Message' });
  // Astryx uses a contenteditable editor, so use its native input boundary.
  input.textContent = '/terminal echo unsafe'; fireEvent.input(input);
  fireEvent.keyDown(input, { key: 'Enter', code: 'Enter' });
  await screen.findByText('Use /terminal without arguments. Shell commands are entered in the terminal.');
  await waitFor(() => expect(input.textContent).toContain('/terminal echo unsafe'));
  input.textContent = '/terminal'; fireEvent.input(input); fireEvent.keyDown(input, { key: 'Enter', code: 'Enter' });
  await screen.findByRole('button', { name: 'Hide terminal' });
  expect(fixture.posts()).toHaveLength(0);
  expect(document.querySelector('[data-nc-card-dock]:not([hidden])')?.getAttribute('data-nc-card-id')).toBe('existing');
});

it('switches existing ordinary terminals without a board double mount or implicit replacement', async () => {
  const fixture = setupWorkbench({ cards: [terminalCard('first'), terminalCard('second')] });
  const mount = vi.spyOn(fixture.runtime.host, 'mount');
  await userEvent.click(await screen.findByRole('button', { name: 'Terminal workbench' }));
  await userEvent.click(within(screen.getByRole('region', { name: 'Workbench' })).getByRole('button', { name: /^second/ }));
  expect(mount).toHaveBeenCalledTimes(2);
  expect(document.querySelectorAll('[data-nc-card-dock]')).toHaveLength(2);
  expect(document.querySelector('[data-nc-card-cell][data-nc-card-id="second"]')).toBeNull();
  await userEvent.click(within(document.querySelector('[data-nc-card-dock]:not([hidden])') as HTMLElement).getByRole('button', { name: /^Delete card / }));
  expect(fixture.requests.filter(request => request.method === 'DELETE')).toHaveLength(0);
  expect(screen.getByRole('dialog', { name: 'Delete this card?' })).toBeTruthy();
  expect(fixture.posts()).toHaveLength(0);
});

it('does not reopen a hidden view or steal changed focus on late completion', async () => {
  const fixture = setupWorkbench({ defer: true });
  const entry = await screen.findByRole('button', { name: 'Terminal workbench' });
  await userEvent.click(entry);
  await waitFor(() => expect(fixture.posts()).toHaveLength(1));
  await userEvent.click(screen.getByRole('button', { name: 'Hide terminal' }));
  entry.focus();
  await act(async () => { fixture.releases[0]?.(); await Promise.resolve(); });
  expect(document.activeElement).toBe(entry);
  expect(screen.queryByRole('button', { name: 'Hide terminal' })).toBeNull();
  expect(fixture.router.state.location.search).not.toHaveProperty('card');
});

it('creates within the resolved daily Track rather than a global or guessed scope', async () => {
  const fixture = setupWorkbench({ daily: true });
  await userEvent.click(await screen.findByRole('button', { name: 'Terminal workbench' }));
  await screen.findByText('No terminal session available.');
  expect(fixture.posts()[0]?.path).toBe('/api/tracks/workbench/terminal-cards');
});

it('leaves a different Track view and its focus untouched when creation completes after navigation', async () => {
  const fixture = setupWorkbench({ defer: true });
  await userEvent.click(await screen.findByRole('button', { name: /^New conversation$/u }));
  const input = await screen.findByRole('combobox', { name: 'Message' });
  input.textContent = '/terminal'; fireEvent.input(input); fireEvent.keyDown(input, { key: 'Enter', code: 'Enter' });
  await waitFor(() => expect(fixture.posts()).toHaveLength(1));
  await act(async () => { await fixture.router.navigate({ to: '/track/$trackId', params: { trackId: 'other' } }); });
  await screen.findAllByText('Other track');
  const destination = await screen.findByRole('button', { name: 'Terminal workbench' });
  destination.focus();
  const location = fixture.router.state.location.href;
  const active = document.activeElement;
  await act(async () => { fixture.releases[0]?.(); await Promise.resolve(); });
  expect(fixture.router.state.location.href).toBe(location);
  expect(document.activeElement).toBe(active);
  expect(screen.queryByRole('button', { name: 'Hide terminal' })).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: /^New conversation$/u }));
  const current = await screen.findByRole('combobox', { name: 'Message' });
  current.textContent = '/terminal'; fireEvent.input(current); fireEvent.keyDown(current, { key: 'Enter', code: 'Enter' });
  await screen.findByText('No terminal session available.');
  expect(fixture.posts().map(post => post.path)).toEqual(['/api/tracks/workbench/terminal-cards', '/api/tracks/other/terminal-cards']);
});

it('routes an ordinary card URL into the same dock and keeps it out of the board', async () => {
  const fixture = setupWorkbench({ cards: [terminalCard('first'), terminalCard('second')] });
  await screen.findByRole('button', { name: 'Terminal workbench' });
  await act(async () => { await fixture.router.navigate({ to: '/track/$trackId', params: { trackId: 'workbench' }, search: { card: 'second' } }); });
  await screen.findByRole('button', { name: 'Hide terminal' });
  expect(document.querySelector('[data-nc-card-dock]:not([hidden])')?.getAttribute('data-nc-card-id')).toBe('second');
  expect(document.querySelector('[data-nc-card-cell][data-nc-card-id="second"]')).toBeNull();
  expect(fixture.posts()).toHaveLength(0);
  expect(fixture.router.state.location.search).toHaveProperty('card', 'second');
});

it('creates an extra terminal only on explicit New, and never overrides a newer selection', async () => {
  const fixture = setupWorkbench({ cards: [terminalCard('first')], defer: true });
  await userEvent.click(await screen.findByRole('button', { name: 'Terminal workbench' }));
  await userEvent.click(screen.getByRole('button', { name: 'Tools' }));
  await userEvent.click(screen.getByRole('menuitem', { name: 'terminal' }));
  await waitFor(() => expect(fixture.posts()).toHaveLength(1));
  await userEvent.click(within(screen.getByRole('region', { name: 'Workbench' })).getByRole('button', { name: /^first/ }));
  await act(async () => { fixture.releases[0]?.(); await Promise.resolve(); });
  expect(document.querySelector('[data-nc-card-dock]:not([hidden])')?.getAttribute('data-nc-card-id')).toBe('first');
  expect(fixture.cards).toHaveLength(2);
  expect(fixture.posts()).toHaveLength(1);
});

it('deletes only after confirmation, removes the inventory row and selects an existing survivor', async () => {
  const fixture = setupWorkbench({ cards: [terminalCard('first'), terminalCard('second')] });
  await userEvent.click(await screen.findByRole('button', { name: 'Terminal workbench' }));
  await userEvent.click(within(screen.getByRole('region', { name: 'Workbench' })).getByRole('button', { name: /^second/ }));
  await userEvent.click(within(document.querySelector('[data-nc-card-dock]:not([hidden])') as HTMLElement).getByRole('button', { name: /^Delete card / }));
  const dialog = screen.getByRole('dialog', { name: 'Delete this card?' });
  await userEvent.click((await import('@testing-library/react')).within(dialog).getByRole('button', { name: 'Delete card' }));
  await waitFor(() => expect(within(screen.getByRole('region', { name: 'Workbench' })).queryByRole('button', { name: /^second/ })).toBeNull());
  expect(fixture.requests.filter(request => request.method === 'DELETE').map(request => request.path)).toEqual(['/api/cards/second']);
  expect(document.querySelector('[data-nc-card-dock]:not([hidden])')?.getAttribute('data-nc-card-id')).toBe('first');
  expect(fixture.posts()).toHaveLength(0);
});

it('rejoins a pending intent after leaving and returning to the Track', async () => {
  const fixture = setupWorkbench({ defer: true });
  await userEvent.click(await screen.findByRole('button', { name: 'Terminal workbench' }));
  await waitFor(() => expect(fixture.posts()).toHaveLength(1));
  await act(async () => { await fixture.router.navigate({ to: '/track/$trackId', params: { trackId: 'other' } }); });
  await screen.findAllByText('Other track');
  await act(async () => { await fixture.router.navigate({ to: '/track/$trackId', params: { trackId: 'workbench' } }); });
  await userEvent.click(await screen.findByRole('button', { name: 'Terminal workbench' }));
  expect(fixture.posts()).toHaveLength(1);
  await act(async () => { fixture.releases[0]?.(); await Promise.resolve(); });
  await screen.findByText('No terminal session available.');
  expect(fixture.posts()).toHaveLength(1);
});

it('keeps the first unknown request across route reentry even when the list now contains its card', async () => {
  const fixture = setupWorkbench({ lost: true });
  await userEvent.click(await screen.findByRole('button', { name: 'Terminal workbench' }));
  await screen.findByRole('alert');
  await act(async () => { await fixture.router.navigate({ to: '/track/$trackId', params: { trackId: 'other' } }); });
  await screen.findAllByText('Other track');
  await act(async () => { await fixture.router.navigate({ to: '/track/$trackId', params: { trackId: 'workbench' } }); });
  await userEvent.click(await screen.findByRole('button', { name: 'Terminal workbench' }));
  await screen.findByText('No terminal session available.');
  expect(fixture.posts()).toHaveLength(2);
  expect(fixture.posts()[1]?.headers).toEqual(fixture.posts()[0]?.headers);
  expect(fixture.posts()[1]?.body).toEqual(fixture.posts()[0]?.body);
});


it.each(['success', 'definite', 'unknown'] as const)('settles %s in a returned owner before reveal without another subscription', async outcome => {
  const fixture = setupWorkbench({ defer: true, failure: outcome === 'success' ? null : outcome });
  await userEvent.click(await screen.findByRole('button', { name: 'Terminal workbench' }));
  await waitFor(() => expect(fixture.posts()).toHaveLength(1));
  await act(async () => { await fixture.router.navigate({ to: '/track/$trackId', params: { trackId: 'other' } }); });
  await screen.findAllByText('Other track');
  await act(async () => { await fixture.router.navigate({ to: '/track/$trackId', params: { trackId: 'workbench' } }); });
  const entry = await screen.findByRole('button', { name: 'Terminal workbench' });
  entry.focus();
  await act(async () => { fixture.releases[0]?.(); await Promise.resolve(); });
  await waitFor(() => expect(screen.queryByText('Creating terminal…')).toBeNull());
  expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Tools' }).disabled).toBe(false);
  expect(document.activeElement).toBe(entry);
  expect(screen.queryByRole('button', { name: 'Hide terminal' })).toBeNull();
  expect(fixture.posts()).toHaveLength(1);
  document.documentElement.dataset.theme = 'light';
  await userEvent.click(entry);
  if (outcome === 'success') {
    expect(fixture.posts()).toHaveLength(1);
  } else {
    await waitFor(() => expect(fixture.posts()).toHaveLength(2));
    expect(fixture.posts()[1]?.headers?.['Idempotency-Key'] === fixture.posts()[0]?.headers?.['Idempotency-Key']).toBe(outcome === 'unknown');
    if (outcome === 'unknown') expect(fixture.posts()[1]?.body).toEqual(fixture.posts()[0]?.body);
    await act(async () => { fixture.releases[1]?.(); await Promise.resolve(); });
  }
  await waitFor(() => expect(screen.queryByText('Creating terminal…')).toBeNull());
  expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Tools' }).disabled).toBe(false);
});

it('retains each unvisited deletion fallback lazily through hide and repeated list changes', async () => {
  const fixture = setupWorkbench({ cards: [terminalCard('first'), terminalCard('second'), terminalCard('third')] });
  const mount = vi.spyOn(fixture.runtime.host, 'mount');
  const entry = await screen.findByRole('button', { name: 'Terminal workbench' });
  expect(mount).toHaveBeenCalledTimes(0);
  await userEvent.click(entry);
  expect(mount).toHaveBeenCalledTimes(1);
  for (const [deleted, survivor, count] of [['first', 'second', 2], ['second', 'third', 3]] as const) {
    await userEvent.click(within(document.querySelector('[data-nc-card-dock]:not([hidden])') as HTMLElement).getByRole('button', { name: /^Delete card / }));
    await userEvent.click(within(screen.getByRole('dialog', { name: 'Delete this card?' })).getByRole('button', { name: 'Delete card' }));
    await waitFor(() => expect(within(screen.getByRole('region', { name: 'Workbench' })).queryByRole('button', { name: new RegExp(`^${deleted}`) })).toBeNull());
    const findDock = () => [...document.querySelectorAll<HTMLElement>('[data-nc-card-dock]')].find(node => node.dataset.ncCardId === survivor) ?? null;
    const dock = findDock();
    expect(dock).not.toBeNull();
    const host = fixture.runtime.host.resolve(survivor);
    await userEvent.click(screen.getByRole('button', { name: 'Hide terminal' }));
    expect(findDock()).toBe(dock);
    expect(fixture.runtime.host.resolve(survivor)).toBe(host);
    await userEvent.click(entry);
    expect(mount).toHaveBeenCalledTimes(count);
  }
  expect(fixture.posts()).toHaveLength(0);
});


it('retains visible list-refresh fallbacks without attaching hidden unvisited terminals', async () => {
  const fixture = setupWorkbench({ cards: [terminalCard('first'), terminalCard('second'), terminalCard('third')] });
  const mount = vi.spyOn(fixture.runtime.host, 'mount');
  const entry = await screen.findByRole('button', { name: 'Terminal workbench' });
  await userEvent.click(entry);
  fixture.cards.splice(0, 1);
  await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.trackDetail('workbench') }); });
  await waitFor(() => expect(document.querySelector('[data-nc-card-dock]:not([hidden])')?.getAttribute('data-nc-card-id')).toBe('second'));
  const second = document.querySelector('[data-nc-card-dock][data-nc-card-id="second"]');
  expect(second).not.toBeNull();
  await userEvent.click(screen.getByRole('button', { name: 'Hide terminal' }));
  expect(document.querySelector('[data-nc-card-dock][data-nc-card-id="second"]')).toBe(second);
  expect(mount).toHaveBeenCalledTimes(2);
  fixture.cards.splice(0, 1);
  await act(async () => { await fixture.client.invalidateQueries({ queryKey: queryKeys.trackDetail('workbench') }); });
  await waitFor(() => expect(document.querySelector('[data-nc-card-dock][data-nc-card-id="second"]')).toBeNull());
  expect(document.querySelector('[data-nc-card-dock][data-nc-card-id="third"]')).toBeNull();
  expect(mount).toHaveBeenCalledTimes(2);
  await userEvent.click(entry);
  const third = document.querySelector('[data-nc-card-dock][data-nc-card-id="third"]');
  expect(third).not.toBeNull();
  await userEvent.click(screen.getByRole('button', { name: 'Hide terminal' }));
  expect(document.querySelector('[data-nc-card-dock][data-nc-card-id="third"]')).toBe(third);
  expect(mount).toHaveBeenCalledTimes(3);
  expect(fixture.posts()).toHaveLength(0);
});


it('keeps the Track conversation final frame during desktop close', async () => {
  setupWorkbench();
  fireEvent.click(await screen.findByRole('button', { name: /^New conversation$/u }));
  const drawer = await screen.findByRole('complementary', { name: 'Untitled' });
  expect(drawer.textContent).toContain('Nothing said yet.');
  fireEvent.click(screen.getByRole('button', { name: /^Close conversation$/u }));
  expect(drawer.hasAttribute('data-nc-drawer')).toBe(true);
  expect(drawer.hidden).toBe(false);
  expect(drawer.textContent).toContain('Nothing said yet.');
  expect(within(drawer).getByRole('heading', { name: 'Untitled' })).toBeTruthy();
});


it.each(['starting', 'exited'] as const)('cold-opens a %s terminal from its URL while secondary reads are held', async status => {
  let release!: () => void;
  const secondaryReads = new Promise<void>(resolve => { release = resolve; });
  const fixture = setupWorkbench({ initialCard: 'cold', secondaryReads, cards: [{ ...terminalCard('cold'),
    runtime: { worker_session_id: 'run-1', kind: 'terminal', status },
  }] });
  try {
    await screen.findByText(status === 'exited' ? 'Session exited.' : 'Starting terminal…');
    expect(fixture.router.state.location.search).toHaveProperty('card', 'cold');
    expect(fixture.client.getQueryData(queryKeys.trackConversations('workbench'))).toBeUndefined();
    await userEvent.click(screen.getByRole('button', { name: 'Hide terminal' }));
    await waitFor(() => expect(fixture.router.state.location.search).not.toHaveProperty('card'));
    await act(async () => { release(); await Promise.resolve(); });
    expect(screen.queryByRole('button', { name: 'Hide terminal' })).toBeNull();
    expect(document.querySelector('[data-nc-terminal-card]')).not.toBeNull();
  } finally { release(); }
});

it('paints one Chat-owned header without a toolbar or letter badge', async () => {
  setupWorkbench({ cards: [terminalCard('shell')] });
  await userEvent.click(await screen.findByRole('button', { name: 'Conversation Main discussion' }));
  await userEvent.click(screen.getByRole('button', { name: 'Terminal workbench' }));
  const workbench = document.querySelector('[data-nc-terminal-workbench]')!;
  const chatHeader = screen.getByRole('complementary', { name: 'Main discussion' }).querySelector('header')!;
  const headers = workbench.querySelectorAll('header');
  expect(headers).toHaveLength(1);
  expect(headers[0].className).toBe(chatHeader.className);
  expect(within(headers[0]).getByRole('heading', { name: 'shell' })).toBeTruthy();
  expect(workbench.querySelector('select')).toBeNull();
  expect([...headers[0].children].map(child => child.tagName)).toEqual(['H2', 'BUTTON', 'BUTTON']);
  expect(workbench.textContent).not.toContain('New terminal');
  expect(workbench.textContent).not.toContain('Delete card');
  expect(within(headers[0]).getByRole('button', { name: 'Delete card shell' })).toBeTruthy();
  expect(within(headers[0]).getByRole('button', { name: 'Hide terminal' })).toBeTruthy();
});

it('omits dock deletion for kernel-owned cards while keeping hide available', async () => {
  const fixture = setupWorkbench({ cards: [{ ...terminalCard('protected'), deletable: false }] });
  await userEvent.click(await screen.findByRole('button', { name: 'Terminal workbench' }));
  const dock = document.querySelector('[data-nc-card-dock]') as HTMLElement;
  expect(within(dock).queryByRole('button', { name: /^Delete card / })).toBeNull();
  await userEvent.click(within(dock).getByRole('button', { name: 'Hide terminal' }));
  expect(document.querySelector('[data-nc-card-dock]')).toBe(dock);
  expect(fixture.requests.some(request => request.method === 'DELETE')).toBe(false);
});

it('does not acknowledge a side reply while Discussion is hidden by Terminal', async () => {
  vi.spyOn(document, 'visibilityState', 'get').mockReturnValue('visible');
  const fixture = setupWorkbench({ cards: [terminalCard('shell')] });
  await screen.findByRole('button', { name: 'Conversation Main discussion' });
  const parent = fixture.client.getQueryData<Conversation[]>(queryKeys.trackConversations('workbench'))![0];
  const side = { ...parent, id: 'side', title: 'Side discussion', sourceCardId: 'parent', lastTurnCompletedAt: 10 };
  const receipts = (id: string) => fixture.requests.filter(request => request.path === `/api/read-receipts/conversation/${id}/read`).map(request => request.body);
  act(() => { fixture.client.setQueryData(queryKeys.trackConversations('workbench'), [parent, side]); });
  fireEvent.click(screen.getByRole('button', { name: 'Conversation Main discussion' }));
  const input = await screen.findByRole('combobox', { name: 'Message' });
  await waitFor(() => expect(input.getAttribute('contenteditable')).toBe('true'));
  await waitFor(() => expect(document.querySelector('[data-nc-thread]')).not.toBeNull());
  input.textContent = '/side'; fireEvent.input(input); fireEvent.keyDown(input, { key: 'Enter', code: 'Enter' });
  await waitFor(() => expect(receipts('side')).toEqual([{ readThroughMs: 10 }]));
  const retained = document.getElementById('conversation-side');
  const sideInput = within(retained!).getByRole('combobox', { name: 'Message' });
  await waitFor(() => expect(sideInput.getAttribute('contenteditable')).toBe('true'));
  sideInput.textContent = 'keep this side draft'; fireEvent.input(sideInput);
  fireEvent.click(screen.getByRole('button', { name: 'Terminal workbench' }));
  await screen.findByRole('button', { name: 'Hide terminal' });
  await waitFor(() => expect(document.querySelector('[data-nc-terminal-card]')).not.toBeNull());
  const host = document.querySelector('[data-nc-terminal-card]');
  expect(retained?.closest('[hidden]')).not.toBeNull();
  // The main receipt proves the same query observer update has committed. Cache
  // publication alone can precede a render and is not a hidden-side assertion.
  for (const at of [20, 30]) {
    act(() => { fixture.client.setQueryData(queryKeys.trackConversations('workbench'),
      [{ ...parent, lastTurnCompletedAt: at }, { ...side, lastTurnCompletedAt: at }]); });
    await waitFor(() => expect(receipts('parent')).toContainEqual({ readThroughMs: at }));
    const visibleReceipts = receipts('parent').length;
    act(() => { document.dispatchEvent(new Event('visibilitychange')); });
    await waitFor(() => expect(receipts('parent')).toHaveLength(visibleReceipts + 1));
    expect(receipts('side')).toEqual([{ readThroughMs: 10 }]);
  }
  fireEvent.click(screen.getByRole('button', { name: 'Discussion' }));
  await waitFor(() => expect(receipts('side')).toEqual([{ readThroughMs: 10 }, { readThroughMs: 30 }]));
  expect(document.getElementById('conversation-side')).toBe(retained);
  expect(document.querySelector('[data-nc-terminal-card]')).toBe(host);
  expect(sideInput.textContent).toBe('keep this side draft');
  fireEvent.click(screen.getByRole('button', { name: 'Discussion' }));
  expect(receipts('side')).toHaveLength(2);
  expect(fixture.requests.filter(request => request.method === 'POST' && !request.path.startsWith('/api/read-receipts/'))).toEqual([]);
});
