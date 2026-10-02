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
      { id: 'end', author: 'turn', turnId: 'turn', status: 'completed', atMs: 3 },
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
      { id: 'end', author: 'turn', turnId: 'turn', status: 'completed', atMs: 3 },
    ]} />);
  expect(regenerate).not.toHaveBeenCalled();
  fireEvent.click(screen.getByRole('button', { name: 'Regenerate response' }));
  await waitFor(() => expect(regenerate).toHaveBeenCalledExactlyOnceWith(user));
  expect(screen.getByText('Answer', { exact: true })).toBeTruthy();
});
