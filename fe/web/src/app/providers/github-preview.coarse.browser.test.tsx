import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { GitHubPreviewProvider } from '../../systems/github-links/public.tsx';
import { Reply } from '../../features/chat/thread/reply.tsx';
import { ProseBlock } from '../../features/report/document/public.tsx';
import '../../styles/entry.css';

afterEach(cleanup);
it('keeps the original chat and report presentation on coarse-pointer devices without GitHub reads', async () => {
  expect(matchMedia('(pointer: coarse)').matches).toBe(true);
  const client = new QueryClient();
  const read = vi.fn(() => Promise.reject(new Error('GitHub should not be read')));
  render(<QueryClientProvider client={client}><GitHubPreviewProvider port={{ read }}>
    <Reply text="[Chat PR](https://github.com/o/r/pull/42)" imageFiles={null} />
    <ProseBlock markdown="[Report PR](https://github.com/o/r/pull/42)" blockId={null} />
  </GitHubPreviewProvider></QueryClientProvider>);
  const chatLink = screen.getByRole('link', { name: 'Chat PR' });
  expect(chatLink.getAttribute('target')).toBe('_blank');
  expect(chatLink.closest('[role="presentation"]')).toBeNull();
  screen.getByRole('button', { name: 'Report PR' }).focus();
  await screen.findByRole('button', { name: 'Load webpage' });
  expect(read).not.toHaveBeenCalled();
  expect(screen.queryByText('Loading GitHub preview…')).toBeNull();
  client.clear();
});
