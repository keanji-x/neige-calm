// The template plugin guides read, through the real adapter over a fake transport: a failed read says the read rule's
// sentence, the server's reason appended only when it refused the read.
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it } from 'vitest';

import type { ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { TemplatePluginGuides } from './template-plugin-guides.tsx';

afterEach(cleanup);

function mount(answers: ApiTransportResponse[]) {
  const transport: ApiTransportPort = { send: () => Promise.resolve(answers.shift() ?? { status: 200, statusText: 'OK', body: [] }) };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  render(<QueryClientProvider client={client}>
    <TemplatePluginGuides templateId="dev" transport={transport} unauthorized={createUnauthorizedChannel({ enqueue: (task) => task() })} />
  </QueryClientProvider>);
}

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
