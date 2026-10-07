import { act, cleanup, fireEvent, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { README_FILE_URL, README_TRACK_ID, README_TRACK_URL, renderReadmeFixture } from './report-readme-fixture.tsx';

beforeEach(() => { vi.spyOn(window, 'scrollTo').mockImplementation(() => undefined); });

afterEach(() => { cleanup(); document.getElementById('root')?.remove(); vi.restoreAllMocks(); });

async function readmeDocument() {
  const file = await screen.findByRole('region', { name: 'File docs/README.md' });
  await within(file).findByRole('heading', { name: 'Documentation', level: 2 });
  expect(within(file).getByRole('heading', { name: 'Use and operate Neige Calm', level: 3 })).toBeTruthy();
  expect(within(file).getByRole('heading', { name: 'Develop and understand the system', level: 3 })).toBeTruthy();
  expect(within(file).getAllByRole('table')).toHaveLength(2);
  expect(within(file).getByRole('button', { name: 'Using Neige Calm' }).getAttribute('title')).toBe('docs/using-neige-calm.md');
  expect(within(file).getByRole('button', { name: 'English README' }).getAttribute('title')).toBe('README.md');
  expect(file.querySelector('pre')).toBeNull();
  expect(file.querySelector('[data-nc-report-file-source]')).toBeNull();
  expect(file.closest('[data-nc-track-page]')).not.toBeNull();
  expect(file.closest('[data-nc-conversation-drawer-host]')).toBeNull();
  expect(file.closest('[inert]')).toBeNull();
  return file;
}

it('cold-loads the exact docs/README.md deep link after track detail arrives', async () => {
  let release = () => {};
  const ready = new Promise<void>(resolve => { release = resolve; });
  const { router, requests } = renderReadmeFixture(README_FILE_URL, ready);
  await waitFor(() => expect(requests.some(request => request.path === `/api/tracks/${README_TRACK_ID}`)).toBe(true));
  expect(requests.some(request => request.path.includes('/readfile'))).toBe(false);
  await act(async () => { release(); await ready; });
  await readmeDocument();
  expect(requests.filter(request => request.path.includes('/readfile')).map(request => request.path))
    .toEqual([`/api/tracks/${README_TRACK_ID}/workspace/readfile?path=docs%2FREADME.md`]);
  expect(router.history.location.href).toBe(README_FILE_URL);
  const back = vi.spyOn(router.history, 'back');
  fireEvent.click(screen.getByRole('button', { name: 'Back to track' }));
  await waitFor(() => expect(router.history.location.href).toBe(README_TRACK_URL));
  expect(back).not.toHaveBeenCalled();
  expect(router.history.length).toBe(1);
});

it('opens from Report with an existing Conversation, closes and returns through history', async () => {
  const { router, requests } = renderReadmeFixture(README_TRACK_URL);
  fireEvent.click(await screen.findByRole('button', { name: /Conversation Existing conversation/ }));
  const conversation = await screen.findByRole('complementary', { name: 'Existing conversation' });
  const input = within(conversation).getByRole('combobox', { name: 'Message' });
  await userEvent.type(input, 'Keep this draft');
  const opener = screen.getByRole('button', { name: 'docs/README.md' });
  await userEvent.click(opener);
  await readmeDocument();
  expect(conversation.isConnected).toBe(true);
  expect(conversation.closest('[inert]')).toBeNull();
  expect(router.history.location.href).toBe(README_FILE_URL);
  fireEvent.click(screen.getByRole('button', { name: 'Back to track' }));
  await waitFor(() => expect(router.history.location.href).toBe(README_TRACK_URL));
  expect(input.textContent).toBe('Keep this draft');
  await waitFor(() => expect(document.activeElement).toBe(opener));
  await userEvent.click(opener);
  await readmeDocument();
  act(() => { router.history.back(); });
  await waitFor(() => expect(screen.queryByRole('region', { name: 'File docs/README.md' })).toBeNull());
  expect(conversation.isConnected).toBe(true);
  expect(input.textContent).toBe('Keep this draft');
  expect(requests.every(request => request.method === 'GET')).toBe(true);
});

it('resolves the real README parent link through the same scoped file navigation', async () => {
  const { router, requests } = renderReadmeFixture(README_FILE_URL);
  const file = await readmeDocument();
  fireEvent.click(within(file).getByRole('button', { name: 'English README' }));
  const parent = await screen.findByRole('region', { name: 'File README.md' });
  await within(parent).findByRole('heading', { name: 'Why Neige Calm?', level: 3 });
  expect(router.history.location.href).toBe(`${README_TRACK_URL}?file=README.md`);
  expect(router.history.length).toBe(1);
  expect(requests.filter(request => request.path.includes('/readfile')).map(request => request.path)).toEqual([
    `/api/tracks/${README_TRACK_ID}/workspace/readfile?path=docs%2FREADME.md`,
    `/api/tracks/${README_TRACK_ID}/workspace/readfile?path=README.md`,
  ]);
});


it('suppresses HTTP 500 details and retries the same workspace read into the real README', async () => {
  const fixture = renderReadmeFixture(README_FILE_URL, undefined, { failFileRead: true, shortReport: true });
  const alert = await screen.findByRole('alert');
  const file = screen.getByRole('region', { name: 'File docs/README.md' });
  expect(alert.textContent).toBe('Could not load this file.Retry');
  expect(within(file).getByText('README.md')).toBeTruthy();
  expect(file.querySelector('[data-nc-report]')).toBeNull();
  fixture.recoverFileRead();
  fireEvent.click(within(file).getByRole('button', { name: 'Retry' }));
  await readmeDocument();
  expect(fixture.requests.filter(request => request.path.includes('/readfile')).map(request => request.path))
    .toEqual(Array<string>(2).fill(`/api/tracks/${README_TRACK_ID}/workspace/readfile?path=docs%2FREADME.md`));
  expect(fixture.router.history.location.href).toBe(README_FILE_URL);
});
