import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { page } from 'vitest/browser';
import type { ReactNode } from 'react';
import type { GitHubPreview } from '../../../../core/domain/github-preview.ts';
import { GitHubPreviewLink, GitHubPreviewProvider } from './public.tsx';
import { Reply } from '../../features/chat/thread/reply.tsx';
import { ProseBlock } from '../../features/report/document/public.tsx';
import '../../styles/entry.css';

beforeEach(async () => { await page.viewport(1280, 800); });
afterEach(() => { cleanup(); document.body.replaceChildren(); });
function summary(): GitHubPreview {
  return { kind: 'pull', number: 42, title: 'Fix link previews', state: 'merged', author: 'octocat',
    labels: ['bug'], excerpt: '<script>alert(1)</script>', changes: { additions: 12, deletions: 3, changed_files: 2 } };
}
function mount(children: ReactNode, read = vi.fn(async () => summary())) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}><GitHubPreviewProvider port={{ read }}>{children}</GitHubPreviewProvider></QueryClientProvider>);
  return { read, client };
}

it('lazily previews production chat links, keeps navigation and reuses the result', async () => {
  const { read, client } = mount(<Reply text="[PR](https://github.com/o/r/pull/42)" imageFiles={null} />);
  const link = screen.getByRole('link', { name: 'PR' });
  expect(link.getAttribute('href')).toBe('https://github.com/o/r/pull/42');
  expect(read).not.toHaveBeenCalled();
  await userEvent.hover(link);
  await screen.findByText('Fix link previews');
  expect(read).toHaveBeenCalledTimes(1);
  expect(screen.getByText('Pull request · merged · octocat')).toBeTruthy();
  expect(screen.getByText('2 files · +12 / −3')).toBeTruthy();
  expect(screen.getByText('<script>alert(1)</script>').querySelector('script')).toBeNull();
  await userEvent.hover(screen.getByRole('link', { name: 'Open in GitHub ↗' }));
  expect(screen.getByRole('dialog')).toBeTruthy();
  await userEvent.keyboard('{Escape}');
  await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  await userEvent.unhover(link);
  await userEvent.hover(link);
  await screen.findByText('Fix link previews');
  expect(read).toHaveBeenCalledTimes(1);
  client.clear();
});

it('previews production report citations on keyboard focus while preserving other external-link policy', async () => {
  const { read, client } = mount(<ProseBlock markdown="[PR](https://github.com/o/r/pull/42) and [Elsewhere](https://example.com)" blockId={null} />);
  expect(screen.queryByRole('link', { name: 'Elsewhere' })).toBeNull();
  const trigger = screen.getByRole('button', { name: 'PR' });
  trigger.focus();
  await screen.findByText('Fix link previews');
  expect(read).toHaveBeenCalledTimes(1);
  expect(screen.getByRole('link', { name: 'Open in GitHub ↗' }).getAttribute('href')).toBe('https://github.com/o/r/pull/42');
  await userEvent.keyboard('{Escape}');
  await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  client.clear();
});

it('renders a failed read safely and retries on deliberate action', async () => {
  const read = vi.fn(async () => summary()).mockRejectedValueOnce(new Error('secret CLI stderr'));
  const { client } = mount(<GitHubPreviewLink href="https://github.com/o/r/pull/42">PR</GitHubPreviewLink>, read);
  await userEvent.hover(screen.getByRole('link', { name: 'PR' }));
  await screen.findByRole('button', { name: 'Retry' });
  expect(document.body.textContent).not.toContain('secret CLI stderr');
  await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
  await screen.findByText('Fix link previews');
  expect(read).toHaveBeenCalledTimes(2);
  client.clear();
});

it('does not fetch unsupported links or compact-view links', async () => {
  const { read, client } = mount(<Reply text="[Site](https://example.com) [PR](https://github.com/o/r/pull/42)" imageFiles={null} />);
  await userEvent.hover(screen.getByRole('link', { name: 'Site' }));
  expect(read).not.toHaveBeenCalled();
  await page.viewport(414, 800);
  await userEvent.hover(screen.getByRole('link', { name: 'PR' }));
  expect(read).not.toHaveBeenCalled();
  expect(screen.queryByRole('dialog')).toBeNull();
  client.clear();
});
