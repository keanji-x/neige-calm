import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { ReportFileViewer } from './public.tsx';

afterEach(cleanup);

it('retries the same failed report file without leaving the report', async () => {
  const readFile = vi.fn().mockRejectedValueOnce(new Error('Permission denied: /repo/note.txt'))
    .mockResolvedValueOnce({ path: '/repo/note.txt', size: 8, text: 'restored', truncated: false });
  render(<ReportFileViewer path="/repo/note.txt" files={{ readFile, rawUrl: (path) => path }}
    fileRoot="/repo" wide onClose={() => {}} />);
  await screen.findByRole('alert');
  await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
  expect(await screen.findByText('restored')).toBeTruthy();
  expect(readFile.mock.calls).toEqual([['/repo/note.txt'], ['/repo/note.txt']]);
});

it('remounts a failed report image for a fresh load', async () => {
  render(<ReportFileViewer path="/repo/image.png" files={{ readFile: vi.fn(), rawUrl: (path) => path }}
    fileRoot="/repo" wide onClose={() => {}} />);
  const image = await screen.findByRole('img');
  fireEvent.error(image);
  await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
  expect(await screen.findByRole('img')).not.toBe(image);
});
