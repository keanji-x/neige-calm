// @vitest-environment jsdom
import { act, cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { markdownExcerpt } from '../../../../../core/markdown/public.ts';
import type { Conversation, ConversationTurn } from '../../../../../core/domain/conversation.ts';
import { ChatThread } from './public.tsx';

vi.mock(import('../../../../../core/markdown/public.ts'), async (original) => {
  const actual = await original();
  return { ...actual, markdownExcerpt: vi.fn(actual.markdownExcerpt) };
});

afterEach(() => { cleanup(); document.querySelector('[data-preview-test-host]')?.remove(); vi.clearAllMocks(); });

const conversation: Conversation = Object.freeze({ id: 'chat', trackId: 'track', trackTitle: 'Measured',
  title: 'Preview', kind: 'codex', state: 'idle', updatedAt: 1, turns: 3 });
function message(id: string, author: 'you' | 'agent', text: string, atMs: number): ConversationTurn {
  return { id, author, text, atMs };
}
function mount(turns: readonly ConversationTurn[]) {
  const host = document.createElement('div');
  host.setAttribute('data-preview-test-host', '');
  host.setAttribute('data-nc-drawer', '');
  const seam = document.createElement('div'); seam.setAttribute('data-nc-drawer-seam', '');
  const pane = document.createElement('div'); pane.setAttribute('data-nc-drawer-scroll', '');
  host.append(pane, seam); document.body.append(host);
  const view = (rows: readonly ConversationTurn[]) => <ChatThread conversation={conversation} turns={rows}
    cards={{}} stalled={false} canContinue={false} />;
  const rendered = render(view(turns), { container: pane });
  return { update: (rows: readonly ConversationTurn[]) => rendered.rerender(view(rows)) };
}
function dots(): HTMLButtonElement[] {
  return [...screen.getByRole('group', { name: 'Jump to an exchange' }).querySelectorAll('button')];
}

it('does not parse hidden history previews while live text grows', () => {
  const stored = [message('q1', 'you', 'Earlier?', 1), message('a1', 'agent', '**Stored answer.**', 2),
    message('q2', 'you', 'Latest?', 3)];
  const view = mount([...stored, message('live', 'agent', '**First live words.**', 4)]);
  expect(markdownExcerpt).not.toHaveBeenCalled();
  view.update([...stored, message('live', 'agent', '**First live words, more.**', 4)]);
  expect(markdownExcerpt).not.toHaveBeenCalled();
});

it('parses only the open preview and updates its current reply without changing focus', async () => {
  const stored = [message('q1', 'you', 'Earlier?', 1), message('a1', 'agent', '**Stored answer.**', 2),
    message('q2', 'you', 'Latest?', 3)];
  const view = mount([...stored, message('live', 'agent', '**First live words.**', 4)]);
  vi.mocked(markdownExcerpt).mockClear();
  act(() => { dots()[0]?.focus(); });
  expect(document.querySelector('[data-nc-rail-preview] p')?.textContent).toBe('Stored answer.');
  expect(vi.mocked(markdownExcerpt).mock.calls.every(([text]) => text === '**Stored answer.**')).toBe(true);
  view.update([...stored, message('live', 'agent', '**First live words, more.**', 4)]);
  expect(vi.mocked(markdownExcerpt).mock.calls.every(([text]) => text === '**Stored answer.**')).toBe(true);
  await userEvent.keyboard('{End}');
  expect(document.querySelector('[data-nc-rail-preview] p')?.textContent).toBe('First live words, more.');
  const focused = document.activeElement;
  view.update([...stored, message('live', 'agent', '**First live words, final.**', 4)]);
  expect(document.querySelector('[data-nc-rail-preview] p')?.textContent).toBe('First live words, final.');
  expect(document.activeElement).toBe(focused);
});

it('keeps the first reply policy and preview identity through history paging and replacement', async () => {
  const latest = [message('q1', 'you', 'Earlier?', 3), message('empty', 'agent', '', 4),
    message('second', 'agent', '**Second answer.**', 5), message('q2', 'you', 'Unanswered?', 6)];
  const view = mount(latest);
  act(() => { dots()[0]?.focus(); });
  expect(document.querySelector('[data-nc-rail-preview] p')).toBeNull();
  const focused = document.activeElement;
  const older = [message('old-q', 'you', 'Older?', 1), message('old-a', 'agent', '**Older answer.**', 2)];
  view.update([...older, ...latest]);
  expect(document.activeElement).toBe(focused);
  expect(document.querySelector('[data-nc-rail-preview] div')?.textContent).toBe('Earlier?');
  expect(document.querySelector('[data-nc-rail-preview] p')).toBeNull();
  view.update([...older, ...latest.filter(entry => entry.id !== 'empty')]);
  expect(document.querySelector('[data-nc-rail-preview] p')?.textContent).toBe('Second answer.');
  await userEvent.keyboard('{End}');
  expect(document.querySelector('[data-nc-rail-preview] div')?.textContent).toBe('Unanswered?');
  expect(document.querySelector('[data-nc-rail-preview] p')).toBeNull();
});
