import { act, cleanup, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { renderPage, track } from './test-fixtures.tsx';

afterEach(cleanup);

it('reopens through the existing mutation before opening the conversation', async () => {
  let finish!: () => void;
  const onReply = vi.fn();
  const onReopenTrack = vi.fn(() => new Promise<void>(resolve => { finish = resolve; }));
  renderPage({ track: track({ closedAt: 5 }), canReopenTrack: true, onReopenTrack, onReply });
  await userEvent.click(screen.getByRole('button', { name: '重新打开并继续' }));
  expect(onReopenTrack).toHaveBeenCalledOnce();
  expect(onReply).not.toHaveBeenCalled();
  await act(async () => { finish(); await Promise.resolve(); });
  await waitFor(() => expect(onReply).toHaveBeenCalledOnce());
  await userEvent.click(screen.getByRole('button', { name: '重新打开并继续' }));
  expect(onReopenTrack).toHaveBeenCalledOnce();
  expect(onReply).toHaveBeenCalledOnce();
});

it('keeps a failed reopen visible and does not open a misleading conversation', async () => {
  const onReply = vi.fn();
  renderPage({ track: track({ closedAt: 5 }), canReopenTrack: true,
    onReopenTrack: vi.fn(() => Promise.reject(new Error('Offline'))), onReply });
  await userEvent.click(screen.getByRole('button', { name: '重新打开并继续' }));
  expect(onReply).not.toHaveBeenCalled();
  expect(await screen.findByRole('alert')).toBeTruthy();
  expect(screen.getByRole('button', { name: '重新打开并继续' }).hasAttribute('disabled')).toBe(false);
});

it('does not offer reopening without the server capability, or while viewing a file', () => {
  const view = renderPage({ track: track({ closedAt: 5 }), onReply: vi.fn() });
  expect(screen.queryByRole('button', { name: '重新打开并继续' })).toBeNull();
  view.unmount();
  renderPage({ track: track({ closedAt: 5 }), canReopenTrack: true, onReply: vi.fn(),
    onCloseBoard: vi.fn(), board: <div>File preview</div> });
  expect(screen.queryByRole('button', { name: '重新打开并继续' })).toBeNull();
});
