import '../../styles/entry.css';
import { cleanup } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import type { ManagedProviderAvailability } from '../../../../core/domain/agent-providers.ts';
import { renderAssistantProviderFixture } from './assistant-provider-fixture.tsx';

const CLAUDE: ManagedProviderAvailability = Object.freeze({
  availability: { provider: 'claude', status: 'ready', reason: null, checked_at_ms: 1, authentication_notice: null },
  capabilities: { managed_conversation: true, images: false, manual_compact: false, history_replacement: false, requires_ready_availability: true },
});
afterEach(async () => { cleanup(); document.getElementById('root')?.remove(); await page.viewport(1280, 800); });

it.each([1280, 390])('selects a dynamically registered backend through the keyboard and freezes retry (%ipx)', async width => {
  await page.viewport(width, 950);
  const fixture = renderAssistantProviderFixture({ answers: [CLAUDE], failFirstCreate: true, mobile: width < 600 });
  if (width < 600) {
    await page.getByRole('button', { name: 'Open conversation history', exact: true }).click();
    await page.getByRole('button', { name: '发起新对话', exact: true }).click();
  } else await page.getByRole('button', { name: 'New conversation', exact: true }).click();
  if (width < 600) await page.getByLabelText('对话选项').click();
  const model = page.getByRole('button', { name: /^Model:/ });
  await model.click();
  await expect.element(page.getByRole('menuitem', { name: 'Selected model' })).toBeVisible();
  expect(fixture.requests.some(request => request.path === '/api/models?provider=claude')).toBe(true);
  expect(fixture.requests.some(request => request.path === '/api/models?provider=codex')).toBe(false);
  await page.getByRole('menuitem', { name: 'Selected model' }).click();
  if (width < 600) await model.click();
  else await page.getByRole('button', { name: /^Reasoning effort:/ }).click();
  await page.getByRole('menuitem', { name: /^high/ }).click();
  await model.click();
  await userEvent.keyboard('{Escape}');
  await expect.element(model).toHaveFocus();
  const field = page.getByRole('combobox', { name: 'Message' });
  await field.fill('Frozen first message');
  await userEvent.keyboard('{Enter}');
  await expect.element(page.getByRole('button', { name: 'Try again', exact: true })).toBeVisible();
  await expect.element(model).toBeDisabled();
  const posts = () => fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/conversations'));
  expect(posts()).toHaveLength(1);
  expect(posts()[0].body).toMatchObject({ provider: 'claude', model: 'selected', reasoning_effort: 'high', text: 'Frozen first message' });
  fixture.client.setQueryData(['managed-providers'], [{ ...CLAUDE, availability: { ...CLAUDE.availability, status: 'unavailable', reason: 'Now unavailable' } }]);
  if (width < 600) await page.getByLabelText('对话选项').click();
  await page.getByRole('button', { name: 'Try again', exact: true }).click();
  await expect.poll(() => posts().length).toBe(2);
  expect(posts()[1].body).toEqual(posts()[0].body);
  expect(posts()[1].headers).toEqual(posts()[0].headers);
  await expect.element(page.getByRole(width < 600 ? 'dialog' : 'complementary', { name: 'Created Assistant' })).toBeVisible();
  expect(fixture.client.getQueryData<{ provider: string }[]>(['track-conversations', 'provider-track'])?.[0].provider).toBe('claude');
  expect(document.documentElement.scrollWidth).toBeLessThanOrEqual(width);
});

it('keeps a dormant backend after fresh-session recovery and takes actions from declared capabilities', async () => {
  await page.viewport(1280, 950);
  const fixture = renderAssistantProviderFixture({ answers: [CLAUDE], existingProvider: 'claude', dormant: true, runAttachmentsSupported: true });
  await page.getByRole('button', { name: 'Conversation Saved Assistant' }).click();
  await expect.element(page.getByText('Saved reply', { exact: true })).toBeVisible();
  const model = page.getByRole('button', { name: /^Model:/ });
  await model.click();
  await expect.element(page.getByRole('menuitem', { name: 'Selected model' })).toBeVisible();
  await expect.element(page.getByRole('group', { name: 'Codex', exact: true })).not.toBeInTheDocument();
  await page.getByRole('menuitem', { name: 'Selected model' }).click();
  await expect.poll(() => fixture.requests.filter(request => request.method === 'PUT' && request.path === '/api/cards/dormant/planner/model').length).toBe(1);
  expect(fixture.requests.find(request => request.method === 'PUT')?.body).toEqual({ model: 'selected', reasoning_effort: null });
  const field = page.getByRole('combobox', { name: 'Message' });
  await field.fill('Resume the declared backend');
  await userEvent.keyboard('{Enter}');
  await page.getByRole('button', { name: 'Start a fresh session' }).click();
  await expect.element(page.getByText(/^Fresh session started/)).toBeVisible();
  await expect.element(page.getByText('Saved reply', { exact: true })).toBeVisible();
  await expect.element(field).toHaveTextContent('Resume the declared backend');
  await field.click(); await userEvent.keyboard('{Enter}');
  await expect.poll(() => fixture.requests.filter(request => request.path.endsWith('/planner/input')).length).toBe(2);
  expect(fixture.client.getQueryData<{ provider: string }[]>(['track-conversations', 'provider-track'])?.[0].provider).toBe('claude');
  await expect.element(page.getByRole('button', { name: /Compact/ })).not.toBeInTheDocument();
  await expect.element(page.getByRole('button', { name: 'Attach an image' })).toBeDisabled();
});

it.each(['unsupported', 'unavailable', 'missing-version', 'old-version'] as const)('refuses %s without sending a fallback', async condition => {
  const answer: ManagedProviderAvailability = condition === 'unsupported'
    ? { ...CLAUDE, capabilities: { ...CLAUDE.capabilities, managed_conversation: false } }
    : condition === 'unavailable'
      ? { ...CLAUDE, availability: { ...CLAUDE.availability, status: 'unavailable', reason: 'Authenticate on this server' } }
      : CLAUDE;
  const fixture = renderAssistantProviderFixture({ answers: [answer], assistantProviders: condition !== 'missing-version', webCompatVersion: condition === 'old-version' ? 50 : 53 });
  await page.getByRole('button', { name: 'New conversation', exact: true }).click();
  if (condition !== 'missing-version' && condition !== 'old-version') {
    await page.getByRole('button', { name: /^Model:/ }).click();
    await expect.element(page.getByText(condition === 'unsupported' ? /does not support managed/ : /Authenticate on this server/)).toBeVisible();
    await userEvent.keyboard('{Escape}');
  }
  const field = page.getByRole('combobox', { name: 'Message' });
  await field.fill('Must not fall back'); await userEvent.keyboard('{Enter}');
  await expect.element(page.getByText(condition === 'missing-version' || condition === 'old-version' ? /does not support Assistant provider/ : /Managed provider capabilities are unavailable/)).toBeVisible();
  expect(fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/conversations'))).toEqual([]);
});


it('creates the declared Codex default despite auth/read and catalog unavailability', async () => {
  const codex: ManagedProviderAvailability = { ...CLAUDE,
    availability: { ...CLAUDE.availability, provider: 'codex', status: 'unavailable', reason: 'Account read failed' },
    capabilities: { ...CLAUDE.capabilities, requires_ready_availability: false } };
  const fixture = renderAssistantProviderFixture({ answers: [codex], catalogUnavailable: true });
  await page.getByRole('button', { name: 'New conversation', exact: true }).click();
  await page.getByRole('button', { name: /^Model:/ }).click();
  await expect.element(page.getByRole('menuitem', { name: /^Default/ })).toBeEnabled();
  await page.getByRole('menuitem', { name: /^Default/ }).click();
  const field = page.getByRole('combobox', { name: 'Message' });
  await field.fill('Follow the backend default'); await userEvent.keyboard('{Enter}');
  await expect.poll(() => fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/conversations')).length).toBe(1);
  expect(fixture.requests.find(request => request.method === 'POST' && request.path.endsWith('/conversations'))?.body).toMatchObject({ provider: 'codex', text: 'Follow the backend default' });
});


it('renders, selects and sends a managed identifier independently of legacy UI labels', async () => {
  const answer: ManagedProviderAvailability = { ...CLAUDE, availability: { ...CLAUDE.availability, provider: 'opencode' } };
  const fixture = renderAssistantProviderFixture({ answers: [answer] });
  await page.getByRole('button', { name: 'New conversation', exact: true }).click();
  const model = page.getByRole('button', { name: /^Model:/ });
  await model.click();
  await expect.element(page.getByRole('group', { name: 'opencode', exact: true })).toBeVisible();
  await page.getByRole('menuitem', { name: 'Selected model' }).click();
  await expect.element(model).toHaveAccessibleName('Model: opencode Selected model');
  await page.getByRole('combobox', { name: 'Message' }).fill('Owner registered backend');
  await userEvent.keyboard('{Enter}');
  await expect.poll(() => fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/conversations')).length).toBe(1);
  expect(fixture.requests.find(request => request.method === 'POST' && request.path.endsWith('/conversations'))?.body).toMatchObject({ provider: 'opencode', model: 'selected' });
});

it.each([false, true])('combines images=%s with a run that supports attachments', async images => {
  const fixture = renderAssistantProviderFixture({ answers: [{ ...CLAUDE, capabilities: { ...CLAUDE.capabilities, images } }],
    existingProvider: 'claude', runAttachmentsSupported: true });
  await page.getByRole('button', { name: 'Conversation Saved Assistant' }).click();
  await expect.element(page.getByText('Saved reply', { exact: true })).toBeVisible();
  await expect.poll(() => fixture.requests.some(request => request.path.endsWith('/planner/run'))).toBe(true);
  const attach = page.getByRole('button', { name: 'Attach an image' });
  if (images) await expect.element(attach).toBeEnabled();
  else await expect.element(attach).toBeDisabled();
  expect(fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/planner/attachments'))).toEqual([]);
});


it('adopts a landed frozen request before stale or missing metadata can refuse it', async () => {
  const options = { answers: [CLAUDE], failFirstCreate: true, landFirstCreate: true, hideLanded: true };
  const fixture = renderAssistantProviderFixture(options);
  await page.getByRole('button', { name: 'New conversation', exact: true }).click();
  await page.getByRole('button', { name: /^Model:/ }).click();
  await page.getByRole('menuitem', { name: 'Selected model' }).click();
  await page.getByRole('combobox', { name: 'Message' }).fill('Already committed');
  await userEvent.keyboard('{Enter}');
  await expect.element(page.getByRole('button', { name: 'Try again', exact: true })).toBeVisible();
  fixture.client.setQueryData(['managed-providers'], []);
  options.answers = []; options.hideLanded = false;
  await page.getByRole('button', { name: 'Try again', exact: true }).click();
  await expect.element(page.getByRole('complementary', { name: 'Created Assistant' })).toBeVisible();
  expect(fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/conversations'))).toHaveLength(1);
});


it('refreshes owner metadata before retrying a draft that never dispatched', async () => {
  const options: { answers: ManagedProviderAvailability[] } = { answers: [] };
  const fixture = renderAssistantProviderFixture(options);
  await page.getByRole('button', { name: 'New conversation', exact: true }).click();
  await page.getByRole('combobox', { name: 'Message' }).fill('Retry after registration');
  await userEvent.keyboard('{Enter}');
  await expect.element(page.getByRole('button', { name: 'Try again', exact: true })).toBeVisible();
  expect(fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/conversations'))).toHaveLength(0);
  options.answers = [{ ...CLAUDE, availability: { ...CLAUDE.availability, provider: 'codex' },
    capabilities: { ...CLAUDE.capabilities, requires_ready_availability: false } }];
  await page.getByRole('button', { name: 'Try again', exact: true }).click();
  await expect.poll(() => fixture.requests.filter(request => request.method === 'POST' && request.path.endsWith('/conversations')).length).toBe(1);
  expect(fixture.requests.some(request => request.path === '/api/agent-providers/managed?refresh=true')).toBe(true);
});
