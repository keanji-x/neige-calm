// The template plugin guides read, through the real adapter over a fake transport: a failed read says the read rule's
// sentence, the server's reason appended only when it refused the read.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render, renderHook, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';

import type { ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { useTemplatePluginGuides } from './template-plugin-guides.tsx';

afterEach(cleanup);

function mount(answers: ApiTransportResponse[]) {
  const transport: ApiTransportPort = { send: () => Promise.resolve(answers.shift() ?? { status: 200, statusText: 'OK', body: [] }) };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
  function Guides() { return useTemplatePluginGuides('dev', transport, unauthorized); }
  render(<QueryClientProvider client={client}>
    <Guides />
  </QueryClientProvider>);
}

it('returns undefined for no template, loading guides and an empty guide read', async () => {
  let resolve!: (response: ApiTransportResponse) => void;
  const answer = new Promise<ApiTransportResponse>(done => { resolve = done; });
  const send = vi.fn(() => answer);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });
  const { result, rerender } = renderHook(({ id }: { id: string | null }) => useTemplatePluginGuides(id, { send }, unauthorized), {
    initialProps: { id: null as string | null },
    wrapper: ({ children }) => <QueryClientProvider client={client}>{children}</QueryClientProvider>,
  });
  expect(result.current).toBeUndefined();
  expect(send).not.toHaveBeenCalled();
  rerender({ id: 'small-change' });
  expect(result.current).toBeUndefined();
  await act(async () => { resolve({ status: 200, statusText: 'OK', body: [] }); await answer; });
  await waitFor(() => { expect(client.isFetching()).toBe(0); });
  rerender({ id: 'small-change' });
  expect(result.current).toBeUndefined();
});

it.each([
  [{ status: 404, statusText: 'Not Found', body: { error: 'template dev', code: 'not_found' } }, 'Could not load template guides. template dev'],
  [{ status: 500, statusText: 'Internal Server Error', body: { error: 'db: disk I/O error', code: 'internal' } }, 'Could not load template guides.'],
] as const)('says a failed guides read by the read rule (%#)', async (answer, text) => {
  mount([answer, { status: 200, statusText: 'OK', body: [{ id: 'gitforge', name: 'development' }] }]);
  const alert = await screen.findByRole('alert');
  expect(alert.firstChild?.textContent).toBe(text);
  await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
  expect(await screen.findByLabelText('development, included by template')).toBeTruthy();
  expect(screen.queryByRole('alert')).toBeNull();
});
