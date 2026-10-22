import { ChatComposer } from '../../features/chat/thread/public.tsx';
import { conversationCommands } from '../../features/chat/thread/composer-command.ts';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, expect, it, vi } from 'vitest';
import { renderCommandFixture } from './composer-command-fixture.tsx';

async function open() {
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Daily Planner conversation/ }));
  const input = await screen.findByRole('combobox', { name: 'Message' });
  await waitFor(() => expect(input.getAttribute('contenteditable')).toBe('true'));
  await screen.findByText('Ready for commands');
  return input;
}
async function type(input: HTMLElement, text: string) {
  input.textContent = text;
  const range = document.createRange();
  range.setStart(input.firstChild!, text.length); range.collapse(true);
  const selection = window.getSelection()!; selection.removeAllRanges(); selection.addRange(range);
  await act(async () => { fireEvent.input(input); await Promise.resolve(); });
}
async function submit(input: HTMLElement, text: string) {
  await type(input, text);
  fireEvent.click(screen.getByRole('button', { name: 'Send' }));
}
afterEach(() => { cleanup(); document.getElementById('root')?.remove(); });

it.each([
  ['open', { closed: true }, { closed: false }],
  ['close', {}, { closed: true }],
] as const)('typed /%s uses the Track mutation', async (command, options, body) => {
  const fixture = renderCommandFixture(options);
  await submit(await open(), `/${command}`);
  await waitFor(() => expect(fixture.requests.filter(request => request.method === 'PATCH').map(request => request.body)).toEqual([body]));
  expect(fixture.requests.filter(request => request.path.endsWith('/planner/input'))).toHaveLength(0);
});

it.each(['open', 'close', 'stop'] as const)('typed unavailable /%s is not sent and retains the draft', async command => {
  const fixture = renderCommandFixture({ canClose: false, canReopen: false });
  const input = await open();
  await type(input, '/');
  await waitFor(() => expect(screen.getAllByRole('option').length).toBeGreaterThan(0));
  expect(screen.queryByRole('option', { name: new RegExp(`^${command}`) })).toBeNull();
  await submit(input, `/${command}`);
  await waitFor(() => expect(input.textContent).toBe(`/${command}`));
  expect(screen.getByRole('alert').textContent).toBeTruthy();
  expect(fixture.requests.filter(request => request.method === 'PATCH' || request.path.endsWith('/planner/input') || request.path.endsWith('/planner/interrupt'))).toHaveLength(0);
});

it('sends an unregistered /xxx as ordinary Planner text', async () => {
  const fixture = renderCommandFixture();
  await submit(await open(), '/xxx a question');
  await waitFor(() => expect(fixture.requests.filter(request => request.path.endsWith('/planner/input'))).toHaveLength(1));
  expect(fixture.requests.find(request => request.path.endsWith('/planner/input'))?.body).toMatchObject({ text: '/xxx a question' });
});

it('menu and typed close share the same execution and the page menu refetch fence', async () => {
  const fixture = renderCommandFixture({ staleDetail: true }); const input = await open();
  await type(input, '/cl');
  await userEvent.click(await screen.findByRole('option', { name: /^close/ }));
  expect(fixture.requests.filter(request => request.method === 'PATCH')).toHaveLength(0);
  fireEvent.keyDown(input, { key: 'Enter' });
  await waitFor(() => expect(fixture.requests.filter(request => request.method === 'PATCH')).toHaveLength(1));
  await submit(input, '/close');
  await waitFor(() => expect(input.textContent).toBe('/close'));
  expect(fixture.requests.filter(request => request.method === 'PATCH')).toHaveLength(1);
  await userEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
  await userEvent.click(screen.getByRole('button', { name: /^Track actions for/ }));
  expect(screen.getByRole('menuitem', { name: 'Close' }).getAttribute('aria-disabled')).toBe('true');
});

it('shows registry order and filters by command-name prefix', async () => {
  renderCommandFixture({ running: true }); const input = await open();
  await type(input, '/');
  await waitFor(() => expect(screen.getAllByRole('option').map(option => option.querySelectorAll('span')[1]?.textContent))
    .toEqual(['stop', 'new', 'side', 'compact', 'terminal', 'close']));
  await type(input, '/cl');
  await waitFor(() => expect(screen.getAllByRole('option').map(option => option.textContent)).toEqual(['closeClose this Track']));
});

it('executes /stop through the same interrupt endpoint as Stop', async () => {
  const fixture = renderCommandFixture({ running: true }); const input = await open();
  await type(input, '/stop');
  await screen.findByRole('option', { name: /^stop/ });
  fireEvent.keyDown(input, { key: 'Enter' });
  fireEvent.keyDown(input, { key: 'Enter' });
  await waitFor(() => expect(fixture.requests.filter(request => request.path.endsWith('/planner/interrupt'))).toHaveLength(1));
  expect(fixture.requests.filter(request => request.path.endsWith('/planner/input'))).toHaveLength(0);
});

it.each([
  ['open', { closed: true }, { closed: false }],
] as const)('menu /%s completes before submit invokes the typed mutation', async (command, options, body) => {
  const fixture = renderCommandFixture(options); const input = await open();
  await type(input, '/');
  await userEvent.click(await screen.findByRole('option', { name: new RegExp(`^${command}`) }));
  expect(fixture.requests.filter(request => request.method === 'PATCH')).toHaveLength(0);
  fireEvent.keyDown(input, { key: 'Enter' });
  await waitFor(() => expect(fixture.requests.filter(request => request.method === 'PATCH').map(request => request.body)).toEqual([body]));
  expect(fixture.requests.filter(request => request.path.endsWith('/planner/input'))).toHaveLength(0);
});

it.each([
  ['close', 'Closing the track is unconfirmed.'],
  ['open', 'Reopening the track is unconfirmed.'],
] as const)('shows /%s mutation failures in the composer and restores the draft', async (command, message) => {
  renderCommandFixture({ closed: command === 'open',
    patch: () => ({ status: 500, statusText: 'Internal Server Error', body: {} }) });
  const input = await open(); await submit(input, `/${command}`);
  await waitFor(() => expect(input.closest('[data-nc-composer]')?.querySelector('[role="alert"]')?.textContent).toContain(message));
  expect(input.textContent).toBe(`/${command}`);
});

it('keeps Track commands out of ordinary conversations and new drafts', async () => {
  renderCommandFixture();
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Other conversation/ }));
  let input = await screen.findByRole('combobox', { name: 'Message' }); await type(input, '/');
  await waitFor(() => expect(screen.getAllByRole('option').length).toBeGreaterThan(0));
  expect(screen.queryByRole('option', { name: /^close/ })).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
  await userEvent.click(screen.getByRole('button', { name: /^New conversation$/ }));
  input = await screen.findByRole('combobox', { name: 'Message' }); await type(input, '/');
  await waitFor(() => expect(screen.getAllByRole('option').length).toBeGreaterThan(0));
  expect(screen.queryByRole('option', { name: /^close/ })).toBeNull();
});


it('retains a typed terminal command on narrow surfaces without a terminal callback', async () => {
  const send = vi.fn();
  render(<ChatComposer onSend={send} commands={conversationCommands({ compactViewport: true })} />);
  const input = screen.getByRole('textbox', { name: 'Message' });
  await submit(input, '/terminal');
  await waitFor(() => expect(input.textContent).toBe('/terminal'));
  expect(screen.getByRole('alert').textContent).toBe('Terminal workbench is available on desktop.');
  expect(send).not.toHaveBeenCalled();
});

it('retains a side question when its registered execution refuses it', async () => {
  const send = vi.fn(); const side = vi.fn(() => false);
  render(<ChatComposer onSend={send} commands={conversationCommands({ side })} />);
  const input = screen.getByRole('combobox', { name: 'Message' });
  await submit(input, '/side Keep my question');
  await waitFor(() => expect(input.textContent).toBe('/side Keep my question'));
  expect(side).toHaveBeenCalledWith('Keep my question');
  expect(send).not.toHaveBeenCalled();
  expect(screen.getByRole('alert').textContent).toContain('not accepted');
});


it.each(['Explain /close', 'Please do not use /stop'])('completes a slash word in prose %s without executing it', async text => {
  const fixture = renderCommandFixture({ running: true }); const input = await open();
  await type(input, text);
  await screen.findByRole('option', { name: text.endsWith('/stop') ? /^stop/ : /^close/ });
  fireEvent.keyDown(input, { key: 'Enter' });
  expect(input.textContent).toBe(text);
  expect(fixture.requests.filter(request => ['PATCH', 'PUT', 'POST'].includes(request.method))).toHaveLength(0);
  fireEvent.keyDown(input, { key: 'Enter' });
  await waitFor(() => expect(fixture.requests.filter(request => request.path.endsWith('/planner/input'))).toHaveLength(1));
  expect(fixture.requests.filter(request => request.method === 'PATCH' || request.method === 'PUT')).toHaveLength(0);
  expect(screen.queryByRole('listbox', { name: 'Commands' })).toBeNull();
});

it('discards a late command failure after switching conversations and restores only its own draft', async () => {
  let fail!: () => void;
  renderCommandFixture({ patch: () => new Promise(resolve => { fail = () => resolve({ status: 500, statusText: 'Internal Server Error', body: {} }); }) });
  let input = await open(); await submit(input, '/close');
  await waitFor(() => expect(fail).toBeTypeOf('function'));
  await userEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
  await userEvent.click(await screen.findByRole('button', { name: /Conversation Other conversation/ }));
  input = await screen.findByRole('combobox', { name: 'Message' }); await type(input, 'Other draft');
  await act(async () => { fail(); await Promise.resolve(); });
  await waitFor(() => expect(input.textContent).toBe('Other draft'));
  expect(input.closest('[data-nc-composer]')?.querySelector('[role="alert"]')).toBeNull();
  await userEvent.click(screen.getByRole('button', { name: 'Close conversation' }));
  input = await open(); await waitFor(() => expect(input.textContent).toBe('/close'));
  expect(input.closest('[data-nc-composer]')?.querySelector('[role="alert"]')).toBeNull();
});

const gestures = ['Enter', 'Tab', 'Shift+Enter', 'Shift+Tab', 'IME Enter', 'click'] as const;
it.each(['close', 'stop'].flatMap(command => gestures.map(gesture => [command, gesture] as const)))(
  'menu /%s with %s only completes; submit then executes once', async (command, gesture) => {
    const fixture = renderCommandFixture({ running: true }); const input = await open();
    await type(input, `/${command}`);
    const option = await screen.findByRole('option', { name: new RegExp(`^${command}`) });
    if (gesture === 'click') await userEvent.click(option);
    else fireEvent.keyDown(input, { key: gesture.includes('Tab') ? 'Tab' : 'Enter',
      shiftKey: gesture.startsWith('Shift'), keyCode: gesture === 'IME Enter' ? 229 : undefined });
    await waitFor(() => expect(screen.queryByRole('listbox', { name: 'Commands' })).toBeNull());
    expect(input.textContent).toBe(`/${command}`);
    expect(fixture.requests.filter(request => ['PATCH', 'PUT', 'POST'].includes(request.method))).toHaveLength(0);
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() => expect(fixture.requests.filter(request => command === 'close' ? request.method === 'PATCH' : request.path.endsWith('/planner/interrupt'))).toHaveLength(1));
    expect(fixture.requests.filter(request => request.path.endsWith('/planner/input'))).toHaveLength(0);
  });

it('middle-of-word menu completion has no writes', async () => {
  const fixture = renderCommandFixture(); const input = await open();
  await type(input, '/close'); await screen.findByRole('option', { name: /^close/ });
  const range = document.createRange(); range.setStart(input.firstChild!, 3); range.collapse(true);
  const selection = window.getSelection()!; selection.removeAllRanges(); selection.addRange(range);
  fireEvent.keyDown(input, { key: 'Tab' });
  expect(fixture.requests.filter(request => ['PATCH', 'PUT', 'POST'].includes(request.method))).toHaveLength(0);
});

it.each(['Explain /close', 'First line\n/close'])(
  'whole draft %s never executes a command', async text => {
    const fixture = renderCommandFixture(); const input = await open();
    await submit(input, text);
    await waitFor(() => expect(fixture.requests.filter(request => request.path.endsWith('/planner/input'))).toHaveLength(1));
    expect(fixture.requests.filter(request => request.method === 'PATCH' || request.method === 'PUT')).toHaveLength(0);
  });


it.each(['/rename New title', '/pin', '/unpin', '/admin', '/member', '/askme', '/sandbox', '/fullaccess'])('sends removed command %s as ordinary text', async text => {
  const fixture = renderCommandFixture(); const input = await open();
  await submit(input, text);
  await waitFor(() => expect(fixture.requests.filter(request => request.path.endsWith('/planner/input')).map(request => request.body))
    .toEqual([expect.objectContaining({ text })]));
  expect(fixture.requests.filter(request => request.method === 'PATCH' || request.method === 'PUT')).toHaveLength(0);
});


it.each(['sent', 'queued'] as const)('editing %s messages never offers the command menu', async edit => {
  renderCommandFixture({ edit, running: edit === 'queued' });
  await open();
  await userEvent.click(await screen.findByRole('button', { name: edit === 'sent' ? 'Edit message' : 'Edit queued message' }));
  const input = screen.getByRole('combobox', { name: 'Message' });
  await type(input, '/');
  expect(screen.queryByRole('listbox', { name: 'Commands' })).toBeNull();
});

it.each((['sent', 'queued'] as const).flatMap(edit => ['/close', '/stop', '/new'].map(text => [edit, text] as const)))(
  'editing %s saves %s through the original message contract without command effects', async (edit, text) => {
    const fixture = renderCommandFixture({ edit, running: edit === 'queued' });
    await open();
    await userEvent.click(await screen.findByRole('button', { name: edit === 'sent' ? 'Edit message' : 'Edit queued message' }));
    const input = screen.getByRole('combobox', { name: 'Message' });
    await type(input, text);
    await userEvent.click(screen.getByRole('button', { name: edit === 'sent' ? 'Replace message' : 'Save queued message' }));
    await waitFor(() => expect(fixture.requests.filter(request => ['POST', 'PATCH', 'PUT', 'DELETE'].includes(request.method))
      .map(({ method, path, body }) => ({ method, path, body }))).toEqual([edit === 'sent'
      ? { method: 'POST', path: '/api/cards/daily-planner/planner/input', body: { text, replaces_turn: 'turn' } }
      : { method: 'PATCH', path: '/api/cards/daily-planner/planner/input/queued', body: { text, if_entry_rev: 2 } }]));
    expect(screen.queryByText('Editing message')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Send first message' })).toBeNull();
  });
