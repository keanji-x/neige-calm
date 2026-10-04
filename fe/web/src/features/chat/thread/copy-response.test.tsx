// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { ThreadStatusNotice } from './status-notice.tsx';
import { ChatThread } from './public.tsx';

afterEach(cleanup);
it('copies the visible response Markdown only after a deliberate click', async () => {
  const copyText = vi.fn().mockResolvedValue(undefined);
  const text = 'Reply\n\n```ts\nx()\n```';
  render(<ChatThread conversation={{ id: 'c', trackId: 't', title: null, kind: 'codex', state: 'idle', updatedAt: 0 }} cards={{}}
    stalled={false} canContinue={false} copyText={copyText} turns={[
      { id: 'u', author: 'you', text: 'Prompt', atMs: 1 }, { id: 'a', author: 'agent', text, atMs: 2 },
      { id: 'end', author: 'turn', elapsedMs: null, turnId: 'turn', status: 'completed', atMs: 3 },
    ]} />);
  expect(copyText).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button', { name: 'Copy response' }));
  await screen.findByRole('button', { name: 'Copied response' });
  expect(copyText).toHaveBeenCalledExactlyOnceWith(text);
});
it('reports rejected copy without fabricating success', async () => {
  render(<ThreadStatusNotice heading="Completed" clock={{ elapsedMs: null, timestamp: null }}
    copyAction={{ id: 'a', text: 'text', run: () => Promise.reject(new Error('Permission denied')) }} />);
  fireEvent.click(screen.getByRole('button', { name: 'Copy response' }));
  await screen.findByRole('button', { name: 'Copy failed: Permission denied' });
  expect(screen.queryByRole('button', { name: 'Copied response' })).toBeNull();
});
it('ignores old copy completion when the response changes', async () => {
  let resolve!: () => void;
  const pending = new Promise<void>((done) => { resolve = done; });
  const view = (id: string) => <ThreadStatusNotice heading="Completed" clock={{ elapsedMs: null, timestamp: null }}
    copyAction={{ id, text: id, run: id === 'a' ? () => pending : () => Promise.resolve() }} />;
  const { rerender } = render(view('a'));
  fireEvent.click(screen.getByRole('button', { name: 'Copy response' }));
  rerender(view('b')); resolve();
  await waitFor(() => expect(screen.getByRole('button', { name: 'Copy response' })).toBeTruthy());
  expect(screen.queryByRole('button', { name: 'Copied response' })).toBeNull();
});

it('does not let a hung old copy block the new response or overwrite its success', async () => {
  let resolveOld!: () => void;
  const old = new Promise<void>((done) => { resolveOld = done; });
  const copy = vi.fn<(text: string) => Promise<void>>().mockImplementationOnce(() => old).mockResolvedValue(undefined);
  const view = (id: string) => <ThreadStatusNotice heading="Completed" clock={{ elapsedMs: null, timestamp: null }}
    copyAction={{ id, text: id, run: () => copy(id) }} />;
  const { rerender } = render(view('a'));
  fireEvent.click(screen.getByRole('button', { name: 'Copy response' }));
  rerender(view('b'));
  fireEvent.click(screen.getByRole('button', { name: 'Copy response' }));
  await screen.findByRole('button', { name: 'Copied response' });
  resolveOld();
  await waitFor(() => expect(copy.mock.calls).toEqual([['a'], ['b']]));
  expect(screen.getByRole('button', { name: 'Copied response' })).toBeTruthy();
});

it('regenerates only the delivered prompt after an explicit click, including its images', async () => {
  const regenerate = vi.fn().mockResolvedValue(undefined);
  const user = { id: 'u', author: 'you' as const, text: 'Prompt', atMs: 1, attachments: [
    { id: 'image', contentType: 'image/png', size: 4, url: '/image' },
  ] };
  render(<ChatThread conversation={{ id: 'c', trackId: 't', title: null, kind: 'codex', state: 'idle', updatedAt: 0 }} cards={{}}
    stalled={false} canContinue={false} regenerateMessage={regenerate} turns={[
      user, { id: 'a', author: 'agent', text: 'Answer', atMs: 2 },
      { id: 'end', author: 'turn', elapsedMs: null, turnId: 'turn', status: 'completed', atMs: 3 },
    ]} />);
  expect(regenerate).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button', { name: 'Regenerate response' }));
  await waitFor(() => expect(regenerate).toHaveBeenCalledExactlyOnceWith(user));
  expect(screen.getByText('Answer', { exact: true })).toBeTruthy();
});

const EDIT_CONVERSATION = { id: 'c', trackId: 't', title: null, kind: 'codex' as const, state: 'idle' as const, updatedAt: 0 };
const editTurns = (first: 'you' | 'system' = 'you') => [
  first === 'you' ? { id: 'u', author: 'you' as const, text: 'Prompt', atMs: 1 }
    : { id: 'wake', author: 'system' as const, label: 'Wake', text: 'Automatic turn', atMs: 1 },
  { id: 'a', author: 'agent' as const, text: 'Answer', atMs: 2 },
  { id: 'end', author: 'turn' as const, turnId: 'turn-7', status: 'completed' as const, atMs: 3, elapsedMs: null },
];

it('hands the current turn’s outcome to Edit at the click; the caller owns the replace', () => {
  const edit = vi.fn();
  render(<ChatThread conversation={EDIT_CONVERSATION} cards={{}} stalled={false} canContinue={false}
    editMessage={edit} turns={editTurns()} />);
  expect(edit).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button', { name: 'Edit message' }));
  expect(edit).toHaveBeenCalledExactlyOnceWith(editTurns()[2]);
  expect(screen.getByText('Answer', { exact: true })).toBeTruthy();
});

it('marks the messages of the turn being edited and leaves the rest alone', () => {
  render(<ChatThread conversation={EDIT_CONVERSATION} cards={{}} stalled={false} canContinue={false}
    editing="end" turns={[{ id: 'u0', author: 'you', text: 'Earlier', atMs: 0 }, { id: 'o0', author: 'turn', turnId: 'turn-6', status: 'completed', elapsedMs: null, atMs: 0 },
      ...editTurns()]} />);
  expect(Array.from(document.querySelectorAll('[data-nc-editing]')).map((said) => said.textContent)).toEqual(['Prompt']);
  expect(screen.getByText('Answer', { exact: true })).toBeTruthy();
});

it.each([
  ['no callback', { editMessage: undefined, cards: {}, turns: editTurns() }],
  ['a live response', { editMessage: vi.fn(), cards: { c: 'working' as const }, turns: editTurns() }],
  ['an automatic turn', { editMessage: vi.fn(), cards: {}, turns: editTurns('system') }],
])('offers no Edit with %s', (_, { editMessage, cards, turns }) => {
  render(<ChatThread conversation={EDIT_CONVERSATION} cards={cards} stalled={false} canContinue={false}
    editMessage={editMessage} turns={turns} />);
  const button = screen.getByRole('button', { name: 'Edit message (not available now)' });
  expect(button.getAttribute('aria-disabled')).toBe('true');
  fireEvent.click(button);
  if (editMessage !== undefined) expect(editMessage).not.toHaveBeenCalled();
});
