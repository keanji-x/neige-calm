import { prepareReportDocument } from '../../features/report/document/preparation.ts';
import '../../styles/entry.css';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider, createMemoryHistory } from '@tanstack/react-router';
import { cleanup, render } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';

afterEach(async () => { cleanup(); await page.viewport(1280, 720); });

function mount(issueGuides?: Promise<ApiTransportResponse>) {
  const requests: ApiRequest[] = [];
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = { send(request) {
    requests.push(request);
    if (request.path === '/api/areas') return Promise.resolve(ok([{ id: 'area', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 }]));
    if (request.path === '/api/track-templates') return Promise.resolve(ok([
      { id: 'dev', title: 'Development', description: null, availability: { status: 'requires_plugin', plugin_id: 'gitforge', plugin_name: 'development' }, tasks: [] },
      { id: 'small-change', title: 'Small change', description: null, availability: { status: 'ready' as const }, tasks: [] },
    ]));
    if (request.path.endsWith('/dev/plugin-guides') && issueGuides !== undefined) return issueGuides;
    if (request.path.endsWith('/plugin-guides')) return Promise.resolve(ok(request.path.includes('/dev/')
      ? [{ id: 'gitforge', name: 'development' }] : []));
    if (request.path.startsWith('/api/track-templates/')) return Promise.resolve(ok({
      id: request.path.split('/').at(-1), title: 'Template', description: null, instructions: null, body: '# Method',
    }));
    if (request.path === '/api/settings') return Promise.resolve(ok({}));
    return Promise.resolve(ok([]));
  } };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({ prepareReport: prepareReportDocument, transport, client, cards: bootTestCardRuntime(),
    unauthorized: createUnauthorizedChannel({ enqueue: task => task() }), onSignOut: () => undefined });
  router.update({ history: createMemoryHistory({ initialEntries: ['/area/area/new'] }) });
  render(<QueryClientProvider client={client}><ThemeProvider><RouterProvider router={router} /></ThemeProvider></QueryClientProvider>);
  return requests;
}

async function chooseTemplate(name: string) {
  await expect.element(page.getByRole('button', { name: /^Template:/ })).toBeVisible();
  await page.getByRole('button', { name: /^Template:/ }).click();
  await page.getByRole('menuitem', { name: new RegExp(`^${name}`) }).click();
}

function inputGeometry() {
  const input = page.getByRole('combobox', { name: 'What this track should do' }).element();
  const box = input.getBoundingClientRect();
  const composer = input.closest('[data-nc-new-track-message]')!.getBoundingClientRect();
  return { box, topOffset: box.top - composer.top };
}

it('places the locked guide above the full-width multiline input at desktop and phone widths', async () => {
  await page.viewport(1440, 900);
  const requests = mount();
  const field = page.getByRole('combobox', { name: 'What this track should do' });
  const message = `Keep this sentence\nKeep this second line\n${'W'.repeat(120)}`;
  await field.fill(message);
  const fullWidths = new Map<number, number>();
  for (const width of [1440, 390, 320]) {
    await page.viewport(width, 844);
    fullWidths.set(width, inputGeometry().box.width);
  }
  await page.viewport(1440, 900);
  await chooseTemplate('Development');
  for (const width of [1440, 390, 320]) {
    await page.viewport(width, 844);
    await expect.element(page.getByLabelText('development, included by template')).toBeVisible();
    expect(document.querySelector('[aria-label="development, included by template"] svg')).not.toBeNull();
    await expect.poll(() => {
      const guide = page.getByLabelText('development, included by template').element().getBoundingClientRect();
      return guide.bottom - inputGeometry().box.top;
    }).toBeLessThanOrEqual(0);
    await expect.poll(() => Math.abs(page.getByLabelText('development, included by template').element()
      .getBoundingClientRect().left - inputGeometry().box.left)).toBeLessThanOrEqual(2);
    expect(Math.abs(inputGeometry().box.width - fullWidths.get(width)!)).toBeLessThanOrEqual(2);

    const input = field.element();
    const box = input.getBoundingClientRect();
    const padding = getComputedStyle(input);
    const left = box.left + parseFloat(padding.paddingLeft);
    const text = document.createRange();
    text.selectNodeContents(input);
    const lines = [...text.getClientRects()].filter(line => line.width > 0);
    expect(lines.length).toBeGreaterThanOrEqual(3);
    for (const line of lines) expect(Math.abs(line.left - left)).toBeLessThanOrEqual(2);
    const textWidth = box.width - parseFloat(padding.paddingLeft) - parseFloat(padding.paddingRight);
    expect(Math.max(...lines.map(line => line.width))).toBeGreaterThan(textWidth * 0.9);
    await expect.element(field).toHaveTextContent('Keep this sentence');
    expect(document.documentElement.scrollWidth).toBe(window.innerWidth);
  }
  await page.viewport(1440, 900);
  await chooseTemplate('No template');
  await expect.element(page.getByLabelText('development, included by template')).not.toBeInTheDocument();
  await expect.element(field).toHaveTextContent('Keep this sentence');
  expect(requests.every(request => request.method === 'GET')).toBe(true);
});

it('leaves no empty composer header for a template without guides or No template', async () => {
  await page.viewport(1440, 900);
  const requests = mount();
  await expect.element(page.getByRole('button', { name: /^Template: No template/ })).toBeVisible();
  expect(requests.some(request => request.path.endsWith('/plugin-guides'))).toBe(false);
  const offsets = new Map<number, number>();
  for (const width of [1440, 390, 320]) {
    await page.viewport(width, 844);
    offsets.set(width, inputGeometry().topOffset);
  }
  await page.viewport(1440, 900);
  await chooseTemplate('Development');
  await expect.element(page.getByLabelText('development, included by template')).toBeVisible();
  await chooseTemplate('Small change');
  await expect.poll(() => requests.some(request => request.path.endsWith('/small-change/plugin-guides'))).toBe(true);
  await expect.element(page.getByRole('group', { name: 'Template plugin guides' })).not.toBeInTheDocument();
  for (const width of [1440, 390, 320]) {
    await page.viewport(width, 844);
    await expect.poll(() => Math.abs(inputGeometry().topOffset - offsets.get(width)!)).toBeLessThanOrEqual(2);
  }
  await page.viewport(1440, 900);
  await chooseTemplate('No template');
  for (const width of [1440, 390, 320]) {
    await page.viewport(width, 844);
    await expect.poll(() => Math.abs(inputGeometry().topOffset - offsets.get(width)!)).toBeLessThanOrEqual(2);
  }
});

it('does not revive a default guide when its read finishes after switching templates', async () => {
  await page.viewport(1440, 900);
  let resolve!: (response: ApiTransportResponse) => void;
  const late = new Promise<ApiTransportResponse>(done => { resolve = done; });
  const requests = mount(late);
  await expect.element(page.getByRole('button', { name: /^Template: No template/ })).toBeVisible();
  const baseline = inputGeometry().topOffset;
  await chooseTemplate('Development');
  await expect.poll(() => requests.some(request => request.path.endsWith('/dev/plugin-guides'))).toBe(true);
  expect(Math.abs(inputGeometry().topOffset - baseline)).toBeLessThanOrEqual(2);
  await chooseTemplate('No template');
  resolve({ status: 200, statusText: 'OK', body: [{ id: 'gitforge', name: 'development' }] });
  await expect.element(page.getByLabelText('development, included by template')).not.toBeInTheDocument();
  await expect.element(page.getByRole('button', { name: /^Template: No template/ })).toBeVisible();
  expect(Math.abs(inputGeometry().topOffset - baseline)).toBeLessThanOrEqual(2);
});
