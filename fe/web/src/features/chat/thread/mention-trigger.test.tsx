import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiAbortSignal } from '../../../../../core/api/types.ts';
import type { MentionSearch, MentionSuggestion } from '../../../../../core/domain/mentions.ts';
import { ChatComposer } from './public.tsx';
import { createMentionSource, mentionToken, useMentionTrigger } from './mention-trigger.tsx';

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

const TAG: MentionSuggestion = {
  id: 'tag:@`tag:部署`', kind: 'tag', label: '#部署', detail: '3 tracks', chip: '#部署', insert: '@`tag:部署`',
};
const TRACK: MentionSuggestion = {
  id: 'track:@`area/reports/Deploy notes.md`', kind: 'track', label: 'Deploy notes', detail: null,
  chip: 'Deploy notes', insert: '@`area/reports/Deploy notes.md`',
};
const BLOCK: MentionSuggestion = {
  id: 'block:@`area/reports/Deploy notes.md#b_1a2b`', kind: 'block', label: 'Rollback', detail: 'Deploy notes',
  chip: 'Deploy notes › Rollback', insert: '@`area/reports/Deploy notes.md#b_1a2b`',
};

type Pending = { query: string; signal: ApiAbortSignal; resolve: (items: readonly MentionSuggestion[]) => void; reject: (error: unknown) => void };

/** A search whose answers the test hands out, in whatever order it likes; it ignores `signal`, as a response already on the wire would. */
function controlledSearch() {
  const calls: Pending[] = [];
  const search: MentionSearch = (query, signal) => new Promise((resolve, reject) => {
    calls.push({ query, signal, resolve, reject });
  });
  return { calls, search };
}

/** Whether `promise` has settled once every queued microtask has run. */
async function settled(promise: Promise<unknown>): Promise<boolean> {
  let done = false;
  void promise.then(() => { done = true; }, () => { done = true; });
  for (let i = 0; i < 10; i += 1) await Promise.resolve();
  return done;
}

describe('createMentionSource', () => {
  it('never lets a superseded answer through, whichever order the answers arrive in', async () => {
    vi.useFakeTimers();
    const { calls, search } = controlledSearch();
    const source = createMentionSource(search, 100);
    const older = Promise.resolve(source.search('dep'));
    await vi.advanceTimersByTimeAsync(100);
    /* Astryx's own order: `cancel()`, then the next search. */
    source.cancel?.();
    const newer = Promise.resolve(source.search('depl'));
    await vi.advanceTimersByTimeAsync(100);
    expect(calls.map((call) => call.query)).toEqual(['dep', 'depl']);
    expect(calls[0].signal.aborted).toBe(true);

    calls[1].resolve([BLOCK]);
    expect((await newer).map((item) => item.id)).toEqual([BLOCK.id]);
    calls[0].resolve([TAG, TRACK]);
    expect(await settled(older)).toBe(false);
  });

  it('supersedes an earlier search even without a cancel in between', async () => {
    vi.useFakeTimers();
    const { calls, search } = controlledSearch();
    const source = createMentionSource(search, 100);
    const older = Promise.resolve(source.search('a'));
    await vi.advanceTimersByTimeAsync(100);
    const newer = Promise.resolve(source.search('ab'));
    await vi.advanceTimersByTimeAsync(100);
    calls[0].resolve([TAG]);
    calls[1].resolve([TRACK]);
    expect((await newer).map((item) => item.id)).toEqual([TRACK.id]);
    expect(await settled(older)).toBe(false);
  });

  it('sends one request per pause in typing, and none for a search cancelled while it waited', async () => {
    vi.useFakeTimers();
    const { calls, search } = controlledSearch();
    const source = createMentionSource(search, 100);
    /* Astryx probes `search('')` on each keystroke only to learn the source is async, then cancels it. */
    void source.search('');
    source.cancel?.();
    void source.search('d');
    await vi.advanceTimersByTimeAsync(50);
    void source.search('');
    source.cancel?.();
    void source.search('de');
    await vi.advanceTimersByTimeAsync(100);
    expect(calls.map((call) => call.query)).toEqual(['de']);
  });

  it('shows headings with at most two ranked examples of each type for bare @', async () => {
    vi.useFakeTimers();
    const { calls, search } = controlledSearch();
    const source = createMentionSource(search, 100);
    const answer = Promise.resolve(source.search(''));
    await vi.advanceTimersByTimeAsync(100);
    expect(calls.map((call) => call.query)).toEqual(['']);
    calls[0].resolve([TAG, TRACK, BLOCK, { ...TAG, id: 'tag:two', label: '#two' }, { ...TAG, id: 'tag:three' }]);
    const items = await answer;
    expect(items.map((item) => item.label).toSorted()).toEqual(
      ['Plugins', 'Tags', TAG.label, '#two', 'More Tags', 'Tracks', TRACK.label, 'More Tracks', 'Blocks', BLOCK.label, 'More Blocks'].toSorted(),
    );
    const tagExamples = items.filter((item) => {
      const data = item.auxiliaryData!;
      return 'suggestion' in data && data.suggestion.kind === 'tag';
    });
    expect(tagExamples.map((item) => item.label)).toEqual([TAG.label, '#two']);
  });

  it('answers a failed search with an empty list instead of throwing', async () => {
    vi.useFakeTimers();
    const { calls, search } = controlledSearch();
    const source = createMentionSource(search, 100);
    const answer = Promise.resolve(source.search('x'));
    await vi.advanceTimersByTimeAsync(100);
    calls[0].reject(new Error('503'));
    expect(await answer).toEqual([]);
  });
});

describe('mentionToken', () => {
  it('serializes exactly the server insert', () => {
    expect(mentionToken(BLOCK).value).toBe(BLOCK.insert);
  });
});

function MentionComposer({ search, onSend, onSteer, onNewConversation }: {
  search: MentionSearch;
  onSend: (text: string) => void;
  onSteer?: (text: string) => void;
  onNewConversation?: () => void;
}) {
  const trigger = useMentionTrigger(search);
  return <ChatComposer onSend={onSend} onSteer={onSteer} mentionTrigger={trigger} {...(onNewConversation === undefined ? {} : { onNewConversation })} />;
}

function field(): HTMLElement {
  return screen.getByRole('combobox', { name: 'Message' });
}

describe('the @ menu in the real composer', () => {
  /* jsdom has no layout; Astryx scrolls the highlighted row into view on every arrow key. */
  const original = Object.getOwnPropertyDescriptor(Element.prototype, 'scrollIntoView');
  beforeEach(() => {
    Object.defineProperty(Element.prototype, 'scrollIntoView', { configurable: true, writable: true, value: () => undefined });
  });
  afterEach(() => {
    if (original === undefined) Reflect.deleteProperty(Element.prototype, 'scrollIntoView');
    else Object.defineProperty(Element.prototype, 'scrollIntoView', original);
  });

  it('enters Tags with a plain @# prefix and searches only tags, then sends the selected mention', async () => {
    const search = vi.fn<MentionSearch>(() => Promise.resolve([TAG]));
    const onSend = vi.fn();
    render(<MentionComposer search={search} onSend={onSend} />);
    await userEvent.type(field(), 'see @');
    await screen.findByRole('option', { name: /^Tags/ });
    expect(screen.getAllByRole('option')).toHaveLength(6);
    const tagIndex = screen.getAllByRole('option').indexOf(screen.getByRole('option', { name: /^Tags/ }));
    await userEvent.keyboard('{ArrowDown}'.repeat(tagIndex) + '{Enter}');
    await screen.findByRole('option', { name: /部署/ });
    expect(field().textContent).toBe('see @#');
    expect(field().querySelector('[data-astryx-token]')).toBeNull();
    expect(search).toHaveBeenLastCalledWith('#', expect.anything());
    expect(onSend).not.toHaveBeenCalled();
    await userEvent.keyboard('{Enter}{Enter}');
    expect(onSend).toHaveBeenLastCalledWith(`see ${TAG.insert}`);
  });

  it('inserts a heading’s example directly without entering its category', async () => {
    const onSend = vi.fn();
    render(<MentionComposer search={() => Promise.resolve([TAG, TRACK, BLOCK])} onSend={onSend} />);
    await userEvent.type(field(), 'see @');
    const example = await screen.findByRole('option', { name: /部署/ });
    await userEvent.click(example);
    expect(field().querySelector('[data-astryx-token]')?.textContent).toBe(TAG.chip);
    expect(onSend).not.toHaveBeenCalled();
    await userEvent.keyboard('{Enter}');
    expect(onSend).toHaveBeenLastCalledWith(`see ${TAG.insert}`);
  });

  it('opens the complete category from its ellipsis without inserting or sending a mention', async () => {
    const search = vi.fn<MentionSearch>(() => Promise.resolve([TAG]));
    const onSend = vi.fn();
    render(<MentionComposer search={search} onSend={onSend} />);
    await userEvent.type(field(), 'see @');
    await userEvent.click(await screen.findByRole('option', { name: 'More Tags' }));
    await screen.findByRole('option', { name: /部署/ });
    expect(field().textContent).toBe('see @#');
    expect(field().querySelector('[data-astryx-token]')).toBeNull();
    expect(search).toHaveBeenLastCalledWith('#', expect.anything());
    expect(onSend).not.toHaveBeenCalled();
  });

  it('selects plugin guides through @+ and sends their documentation verbatim', async () => {
    const plugin: MentionSuggestion = {
      id: 'plugin:dev.example', kind: 'plugin', label: 'Development', detail: 'Issue workflow',
      chip: 'Development', insert: 'Plugin reference (documentation only; kernel permissions still apply): ' + JSON.stringify({ id: 'dev.example', name: 'Development', description: 'Issue workflow' }),
    };
    const search = vi.fn<MentionSearch>((query) => Promise.resolve(query.startsWith('+') ? [plugin] : [TAG, plugin]));
    const onSend = vi.fn();
    render(<MentionComposer search={search} onSend={onSend} />);
    await userEvent.type(field(), 'see @');
    await userEvent.click(await screen.findByRole('option', { name: 'More Plugins' }));
    await screen.findByRole('option', { name: /Development/ });
    expect(field().textContent).toBe('see @+');
    expect(search).toHaveBeenLastCalledWith('+', expect.anything());
    await userEvent.keyboard('{Enter}{Enter}');
    expect(onSend).toHaveBeenLastCalledWith(`see ${plugin.insert}`);
  });

  it('opens an empty Plugins category without querying other mention types', async () => {
    const search = vi.fn<MentionSearch>((query) => Promise.resolve(query === '+' ? [] : [TAG, TRACK, BLOCK]));
    render(<MentionComposer search={search} onSend={vi.fn()} />);
    await userEvent.type(field(), '@');
    const plugins = await screen.findByRole('option', { name: /^Plugins/ });
    const pluginIndex = screen.getAllByRole('option').indexOf(plugins);
    await userEvent.keyboard('{ArrowDown}'.repeat(pluginIndex) + '{Enter}');
    await screen.findByText('No matches');
    expect(field().textContent).toBe('@+');
    expect(search.mock.calls.map(([query]) => query)).toEqual(['', '+']);
    expect(screen.queryByRole('option')).toBeNull();
  });

  it('groups the answer under Tags, Tracks and Blocks and sends exactly the picked insert', async () => {
    const search = vi.fn<MentionSearch>(() => Promise.resolve([TAG, TRACK, BLOCK]));
    const onSend = vi.fn();
    render(<MentionComposer search={search} onSend={onSend} />);
    await userEvent.type(field(), 'see @dep');

    const menu = await screen.findByRole('listbox', { name: 'Mention' });
    /* The listbox opens on "Searching…"; the rows follow the source's own delay. */
    await within(menu).findAllByRole('option');
    expect(within(menu).getAllByRole('group').map((group) => group.getAttribute('aria-label')))
      .toEqual(['Tags', 'Tracks', 'Blocks']);
    expect(within(within(menu).getByRole('group', { name: 'Blocks' })).getByRole('option').textContent)
      .toBe('RollbackDeploy notes');
    expect(search).toHaveBeenLastCalledWith('dep', expect.anything());

    await userEvent.keyboard('{ArrowDown}{ArrowDown}{Enter}');
    await waitFor(() => { expect(screen.queryByRole('listbox')).toBeNull(); });
    const chip = field().querySelector('[data-astryx-token]');
    expect(chip?.textContent).toBe('Deploy notes › Rollback');
    expect(onSend).not.toHaveBeenCalled();

    await userEvent.keyboard('{Enter}');
    expect(onSend).toHaveBeenCalledOnce();
    expect(onSend.mock.calls[0][0]).toBe('see @`area/reports/Deploy notes.md#b_1a2b`');
  });

  it('shows the newest query\'s answer when an older one arrives after it', async () => {
    const { calls, search } = controlledSearch();
    render(<MentionComposer search={search} onSend={vi.fn()} />);
    await userEvent.type(field(), '@a');
    await waitFor(() => { expect(calls.map((call) => call.query)).toContain('a'); });
    await userEvent.type(field(), 'b');
    await waitFor(() => { expect(calls.map((call) => call.query)).toContain('ab'); });
    const byQuery = (query: string) => calls.find((call) => call.query === query)!;

    act(() => { byQuery('ab').resolve([BLOCK]); });
    await screen.findByRole('option', { name: /Rollback/ });
    byQuery('a').resolve([TAG]);
    await act(() => new Promise((resolve) => { setTimeout(resolve, 0); }));

    const options = screen.getAllByRole('option');
    expect(options.map((option) => option.textContent)).toEqual(['RollbackDeploy notes']);
  });

  it('does not pick the last query\'s row with Enter or Tab while the next query is searching', async () => {
    const { calls, search } = controlledSearch();
    const onSend = vi.fn();
    render(<MentionComposer search={search} onSend={onSend} />);
    await userEvent.type(field(), '@a');
    await waitFor(() => { expect(calls.map((call) => call.query)).toContain('a'); });
    act(() => { calls.find((call) => call.query === 'a')!.resolve([TAG]); });
    await screen.findByRole('option', { name: /部署/ });

    await userEvent.type(field(), 'zzz');
    await screen.findByText('Searching…');
    await userEvent.keyboard('{Enter}');
    await userEvent.keyboard('{Tab}');
    expect(field().querySelector('[data-astryx-token]')).toBeNull();
    expect(field().textContent).toBe('@azzz');
    expect(onSend).not.toHaveBeenCalled();
  });

  it('sends on Enter, and lets Tab go, while an open menu has nothing to pick', async () => {
    const onSend = vi.fn();
    render(<MentionComposer search={() => Promise.resolve([])} onSend={onSend} onNewConversation={vi.fn()} />);
    await userEvent.type(field(), 'check /tmp/x');
    await screen.findByText('No command by that name');
    expect(fireEvent.keyDown(field(), { key: 'Tab' })).toBe(true);
    await userEvent.keyboard('{Enter}');
    expect(onSend).toHaveBeenLastCalledWith('check /tmp/x');

    await userEvent.type(field(), 'ask @bob');
    await screen.findByText('No matches');
    expect(fireEvent.keyDown(field(), { key: 'Tab' })).toBe(true);
    await userEvent.keyboard('{Enter}');
    expect(onSend).toHaveBeenLastCalledWith('ask @bob');
  });

  it('after a send over an empty @ menu, the next @ pick inserts its chip', async () => {
    const onSend = vi.fn();
    const search = vi.fn<MentionSearch>((query) => Promise.resolve(query === 'zz' ? [TAG] : []));
    render(<MentionComposer search={search} onSend={onSend} />);
    await userEvent.type(field(), 'ask @bob');
    await screen.findByText('No matches');
    await userEvent.keyboard('{Enter}');
    expect(onSend).toHaveBeenLastCalledWith('ask @bob');
    await waitFor(() => { expect(field().getAttribute('aria-expanded')).toBe('false'); });

    await userEvent.type(field(), '@zz');
    await screen.findByRole('option', { name: /部署/ });
    await userEvent.keyboard('{Enter}');
    expect(field().querySelector('[data-astryx-token]')?.textContent).toBe('#部署');
    await userEvent.keyboard('{Enter}');
    expect(onSend).toHaveBeenLastCalledWith(TAG.insert);
  });

  it('after a send over an empty / menu, /new still runs', async () => {
    const onSend = vi.fn();
    const onNewConversation = vi.fn();
    render(<MentionComposer search={() => Promise.resolve([])} onSend={onSend} onNewConversation={onNewConversation} />);
    await userEvent.type(field(), 'check /tmp/x');
    await screen.findByText('No command by that name');
    await userEvent.keyboard('{Enter}');
    expect(onSend).toHaveBeenLastCalledWith('check /tmp/x');

    await userEvent.type(field(), '/');
    await screen.findByRole('option', { name: /^new/ });
    await userEvent.keyboard('{Enter}');
    expect(onNewConversation).toHaveBeenCalledOnce();
    expect(field().textContent).toBe('');
  });

  it('keeps @/ in the @ menu with the raw text for the search, and never opens the / commands', async () => {
    const onSend = vi.fn();
    const onNewConversation = vi.fn();
    const search = vi.fn<MentionSearch>(() => Promise.resolve([TRACK]));
    render(<MentionComposer search={search} onSend={onSend} onNewConversation={onNewConversation} />);
    const commandsClosed = () => {
      expect(screen.queryByRole('listbox', { name: 'Commands' })).toBeNull();
      expect(screen.queryByRole('option', { name: /^new/ })).toBeNull();
    };

    await userEvent.type(field(), 'see @/');
    await within(await screen.findByRole('listbox', { name: 'Mention' })).findByRole('option', { name: /Deploy notes/ });
    expect(search).toHaveBeenLastCalledWith('/', expect.anything());
    commandsClosed();

    await userEvent.type(field(), 'dep');
    await waitFor(() => { expect(search).toHaveBeenLastCalledWith('/dep', expect.anything()); });
    await within(screen.getByRole('listbox', { name: 'Mention' })).findByRole('option', { name: /Deploy notes/ });
    commandsClosed();
    await userEvent.keyboard('{Enter}');
    expect(field().querySelector('[data-astryx-token]')?.textContent).toBe('Deploy notes');
    await userEvent.keyboard('{Enter}');
    expect(onSend).toHaveBeenLastCalledWith(`see ${TRACK.insert}`);
    expect(onNewConversation).not.toHaveBeenCalled();

    await userEvent.type(field(), '/new');
    await userEvent.keyboard('{Enter}');
    expect(onNewConversation).toHaveBeenCalledOnce();
  });

  it('shows the empty text when the search fails', async () => {
    render(<MentionComposer search={() => Promise.reject(new Error('offline'))} onSend={vi.fn()} />);
    await userEvent.type(field(), '@x');
    expect(await screen.findByText('No matches')).toBeTruthy();
  });

  it('keeps the / command beside it', async () => {
    const onNewConversation = vi.fn();
    render(<MentionComposer search={vi.fn<MentionSearch>(() => Promise.resolve([]))} onSend={vi.fn()} onNewConversation={onNewConversation} />);
    await userEvent.type(field(), '/');
    expect(screen.getByRole('option', { name: /^new/ })).toBeTruthy();
    await userEvent.keyboard('{Enter}');
    expect(onNewConversation).toHaveBeenCalledOnce();
  });

  it('is not there without a search: the field stays a plain textbox', () => {
    function Plain() {
      const trigger = useMentionTrigger(null);
      return <ChatComposer onSend={vi.fn()} mentionTrigger={trigger} />;
    }
    render(<Plain />);
    expect(screen.getByRole('textbox', { name: 'Message' })).toBeTruthy();
    expect(screen.queryByRole('combobox')).toBeNull();
  });
});

it('clears an empty mention menu before the direct steer shortcut', async () => {
  const onSteer = vi.fn();
  const search = vi.fn<MentionSearch>((query) => Promise.resolve(query === 'zz' ? [TAG] : []));
  render(<MentionComposer search={search} onSend={vi.fn()} onSteer={onSteer} />);
  await userEvent.type(field(), 'ask @bob');
  await screen.findByText('No matches');
  fireEvent.keyDown(field(), { key: 'Enter', ctrlKey: true, shiftKey: true });
  expect(onSteer).toHaveBeenCalledWith('ask @bob');
  await waitFor(() => { expect(field().getAttribute('aria-expanded')).toBe('false'); });
  await userEvent.type(field(), '@zz');
  await screen.findByRole('option', { name: /部署/ });
  await userEvent.keyboard('{Enter}');
  expect(field().querySelector('[data-astryx-token]')?.textContent).toBe('#部署');
});
