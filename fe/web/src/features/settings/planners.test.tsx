import userEvent from '@testing-library/user-event';
// @vitest-environment jsdom
import { act, cleanup, fireEvent, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { ApiError } from '../../../../core/domain/failure-class.ts';
import { PlannersPane, type PlannersPaneProps } from './planners.tsx';

afterEach(cleanup);

const PROVIDERS: NonNullable<PlannersPaneProps['providers']> = [
  { availability: { provider: 'codex', status: 'ready', reason: null, authentication_notice: null, checked_at_ms: 1_760_000_000_000 }, accepts_unavailable_blank_track: true },
  { availability: { provider: 'claude', status: 'not_configured', reason: 'calm-server was started without --claude-planner-config',
    authentication_notice: null, checked_at_ms: 1_760_000_000_000 }, accepts_unavailable_blank_track: true },
];

function pane(overrides: Partial<PlannersPaneProps> = {}) {
  const props: PlannersPaneProps = {
    providers: PROVIDERS, loadError: null, onRetryLoad: vi.fn(), onRecheck: vi.fn(), rechecking: false,
    recheckError: null, authenticationRecovery: null,
    turnDuration: { readRevision: 1, value: undefined, loaded: true, loadError: null, onRetryLoad: vi.fn(), onSave: vi.fn().mockResolvedValue(undefined) }, ...overrides,
  };
  const view = render(<PlannersPane {...props} />);
  return { ...props, rerender: view.rerender };
}

function row(title: string): HTMLElement {
  const item = screen.getByText(title).closest('li');
  if (item === null) throw new Error(`no row for ${title}`);
  return item;
}

describe('PlannersPane', () => {
  it('says it is checking rather than guessing a status', () => {
    pane({ providers: undefined });
    expect(screen.getByText('Checking providers…')).toBeTruthy();
    expect(screen.queryByText('Ready')).toBeNull();
    expect(screen.queryByRole('button', { name: 'Recheck planners' })).toBeNull();
  });

  it('shows each provider with its status, and the server reason when it is not ready', () => {
    pane();
    expect(within(row('Codex')).getByText('Ready')).toBeTruthy();
    expect(within(row('Codex')).getByText('Passed every check; new tracks can use it.')).toBeTruthy();
    expect(within(row('Claude')).getByText('Not configured')).toBeTruthy();
    expect(within(row('Claude')).getByText('calm-server was started without --claude-planner-config')).toBeTruthy();
  });

  it('describes blank track creation during Codex and Claude outages (#3037)', () => {
    pane({ providers: [
      { availability: { provider: 'codex', status: 'unavailable', reason: 'shared codex app-server is not running', authentication_notice: null, checked_at_ms: 1 }, accepts_unavailable_blank_track: true },
      { availability: { provider: 'claude', status: 'unavailable', reason: 'not logged in', authentication_notice: null, checked_at_ms: 1 }, accepts_unavailable_blank_track: true },
    ] });
    expect(within(row('Codex')).getByText(
      'shared codex app-server is not running A track can still be created; it runs once Codex is back.',
    )).toBeTruthy();
    expect(within(row('Claude')).getByText('not logged in A track can still be created; it runs once Claude is back.')).toBeTruthy();
  });

  it.each([false, true])('uses injected blank-creation capability %s independently of provider identity', (acceptsUnavailable) => {
    pane({ providers: PROVIDERS.map(({ availability }) => ({
      availability: { ...availability, status: 'unavailable', reason: 'server outage' },
      accepts_unavailable_blank_track: acceptsUnavailable,
    })) });
    for (const title of ['Codex', 'Claude']) {
      expect(within(row(title)).getByText(acceptsUnavailable
        ? `server outage A track can still be created; it runs once ${title} is back.` : 'server outage')).toBeTruthy();
    }
  });

  it('retains server order, the oldest check and notice precedence over outage copy', () => {
    pane({ providers: [
      { ...PROVIDERS[1], availability: { provider: 'opencode', status: 'not_configured', reason: 'not configured', checked_at_ms: 3000, authentication_notice: null } },
      { ...PROVIDERS[0], availability: { provider: 'codex', status: 'unavailable', reason: 'daemon down', checked_at_ms: 1000,
        authentication_notice: { kind: 'refresh_error_reported', text: 'reported notice', revision: '9' } } },
    ] });
    expect(screen.getAllByRole('listitem').map((item) => within(item).queryByText(/^(OpenCode|Codex)$/)?.textContent)
      .filter(Boolean)).toEqual(['OpenCode', 'Codex']);
    expect(screen.getByText(`Last checked at ${new Date(1000).toLocaleTimeString()}. The server keeps an answer for 30 seconds.`)).toBeTruthy();
    expect(within(row('Codex')).getByText('reported notice')).toBeTruthy();
    expect(screen.queryByText(/daemon down/)).toBeNull();
    expect(screen.queryByRole('button', { name: 'Retry queued messages after server sign-in' })).toBeNull();
  });

  it('names Grok and shows its sign-in advice when it is unavailable', () => {
    const reason = 'grok did not authenticate (authenticate `cached_token`: timed out); sign it in with its own CLI under the environment its ACP registration declares, then retry';
    pane({ providers: [
      { availability: { provider: 'grok', status: 'unavailable', reason, authentication_notice: null, checked_at_ms: 1 }, accepts_unavailable_blank_track: true },
    ] });
    expect(within(row('Grok')).getByText(`${reason} A track can still be created; it runs once Grok is back.`)).toBeTruthy();
  });

  it('rechecks on request and says when a recheck failed', () => {
    const props = pane({ recheckError: 'network down' });
    fireEvent.click(screen.getByRole('button', { name: 'Recheck planners' }));
    expect(props.onRecheck).toHaveBeenCalledTimes(1);
    expect(screen.getByRole('alert').textContent).toBe('network down');
  });

  it('offers a retry when the first read failed', () => {
    const props = pane({ providers: undefined, loadError: 'boom' });
    fireEvent.click(screen.getByRole('button', { name: /retry/i }));
    expect(props.onRetryLoad).toHaveBeenCalledTimes(1);
  });
});

it('offers an injected owner retry only for a confirmed failure and passes its exact revision', async () => {
  const onRetry = vi.fn();
  pane({
    providers: [{ availability: { provider: 'codex', status: 'unavailable', reason: 'Sign in for the server.', checked_at_ms: 1,
      authentication_notice: { kind: 'sign_in_required', text: 'Sign in for the server.', revision: '7' } },
      accepts_unavailable_blank_track: true }],
    authenticationRecovery: { provider: 'codex', pending: false, error: null, notices: [], onRetry, onOpenConversation: vi.fn() },
  });
  await userEvent.click(screen.getByRole('button', { name: 'Retry queued messages after server sign-in' }));
  expect(onRetry).toHaveBeenCalledWith('7');
});


it('shows the default, saves valid minutes on Enter once, and resets explicitly', async () => {
  const props = pane();
  const field = screen.getByRole('textbox', { name: 'Maximum turn duration (minutes)' });
  expect((field as HTMLInputElement).value).toBe('30');
  expect(screen.getByText(/Backends without steer support/)).toBeTruthy();
  fireEvent.change(field, { target: { value: '45' } });
  fireEvent.keyDown(field, { key: 'Enter' });
  fireEvent.blur(field);
  expect(props.turnDuration.onSave).toHaveBeenCalledTimes(1);
  expect(props.turnDuration.onSave).toHaveBeenCalledWith('45');
  await screen.findByText('Saved.');
  fireEvent.click(screen.getByRole('button', { name: 'Restore default duration' }));
  expect(props.turnDuration.onSave).toHaveBeenLastCalledWith(null);
});

it('associates help and errors with corrupt stored and invalid drafts, then clears invalid on repair/reset', async () => {
  const props = pane({ turnDuration: { readRevision: 1, value: 'broken', loaded: true, loadError: null,
    onRetryLoad: vi.fn(), onSave: vi.fn().mockResolvedValue(undefined) } });
  const field = screen.getByRole('textbox', { name: 'Maximum turn duration (minutes)' });
  const description = () => {
    const ids = field.getAttribute('aria-describedby');
    expect(ids).toBeTruthy();
    return ids!.split(/\s+/).map((id) => {
      const element = document.getElementById(id);
      expect(element).not.toBeNull();
      return element!.textContent;
    }).join(' ');
  };
  expect(field.getAttribute('aria-invalid')).toBe('true');
  for (const phrase of ['Default: 30', '1–240', 'next turn', 'retrying an unconfirmed start keeps its frozen budget', 'Hold waiting', 'Backends without steer support', 'Stored duration is invalid']) expect(description()).toContain(phrase);
  fireEvent.change(field, { target: { value: '241' } });
  fireEvent.keyDown(field, { key: 'Enter' });
  expect(field.getAttribute('aria-invalid')).toBe('true');
  expect(description()).toContain('1 to 240');
  expect(props.turnDuration.onSave).not.toHaveBeenCalled();
  fireEvent.change(field, { target: { value: '60' } });
  expect(field.getAttribute('aria-invalid')).toBeNull();
  expect(description()).not.toContain('Stored duration is invalid');
  fireEvent.change(field, { target: { value: '241' } });
  fireEvent.click(screen.getByRole('button', { name: 'Restore default duration' }));
  expect(field.getAttribute('aria-invalid')).toBeNull();
  expect((field as HTMLInputElement).value).toBe('30');
  expect(props.turnDuration.onSave).toHaveBeenCalledWith(null);
  await screen.findByText('Saved.');
});


it('does not resend an acknowledged value after the authoritative query echo', async () => {
  const props = pane();
  const field = screen.getByRole('textbox', { name: 'Maximum turn duration (minutes)' });
  fireEvent.change(field, { target: { value: '45' } });
  fireEvent.keyDown(field, { key: 'Enter' });
  await screen.findByText('Saved.');
  props.rerender(<PlannersPane {...props} turnDuration={{ ...props.turnDuration, value: '45' }} />);
  fireEvent.blur(field);
  expect(props.turnDuration.onSave).toHaveBeenCalledTimes(1);
});

it('retains a rapid edit through a pending write and external refresh without queuing a PUT', async () => {
  let finish!: () => void;
  const pending = new Promise<void>((resolve) => { finish = resolve; });
  const onSave = vi.fn().mockReturnValueOnce(pending).mockResolvedValue(undefined);
  const props = pane({ turnDuration: { readRevision: 1, loaded: true, value: undefined, loadError: null, onRetryLoad: vi.fn(), onSave } });
  const field = screen.getByRole('textbox', { name: 'Maximum turn duration (minutes)' });
  fireEvent.change(field, { target: { value: '45' } });
  fireEvent.keyDown(field, { key: 'Enter' });
  fireEvent.change(field, { target: { value: '60' } });
  fireEvent.keyDown(field, { key: 'Enter' });
  fireEvent.blur(field);
  expect(onSave).toHaveBeenCalledTimes(1);
  expect(screen.getByText('Saving 45 minutes…')).toBeTruthy();
  props.rerender(<PlannersPane {...props} turnDuration={{ ...props.turnDuration, value: '90' }} />);
  expect((field as HTMLInputElement).value).toBe('60');
  expect(screen.getByText(/Changed elsewhere/)).toBeTruthy();
  await act(async () => { finish(); await pending; });
  expect(screen.queryByText('Saved.')).toBeNull();
  expect(onSave).toHaveBeenCalledTimes(1);
  expect(screen.getByText('Your edit is not saved.')).toBeTruthy();
  fireEvent.keyDown(field, { key: 'Enter' });
  await screen.findByText('Saved.');
  expect(onSave.mock.calls).toEqual([['45'], ['60']]);
});

it.each([
  ['refused', new ApiError({ kind: 'http', status: 400, code: 'bad_request', message: 'Duration refused', body: {} }), 'Duration refused'],
  ['lost', new Error('socket secret'), 'The save is unconfirmed.'],
])('shows a %s save and retries only on an explicit Enter', async (_label, error, sentence) => {
  const onSave = vi.fn().mockRejectedValueOnce(error).mockResolvedValue(undefined);
  const props = pane({ turnDuration: { readRevision: 1, loaded: true, value: undefined, loadError: null, onRetryLoad: vi.fn(), onSave } });
  const field = screen.getByRole('textbox', { name: 'Maximum turn duration (minutes)' });
  fireEvent.change(field, { target: { value: '45' } });
  fireEvent.keyDown(field, { key: 'Enter' });
  expect((await within(row('Maximum turn duration')).findByRole('alert')).textContent).toContain(sentence);
  expect(within(row('Maximum turn duration')).getByRole('alert').textContent).not.toContain('socket secret');
  fireEvent.blur(field);
  expect(onSave).toHaveBeenCalledTimes(1);
  props.rerender(<PlannersPane {...props} turnDuration={{ ...props.turnDuration, value: '30' }} />);
  expect(within(row('Maximum turn duration')).getByRole('alert').textContent).toContain(sentence);
  fireEvent.keyDown(field, { key: 'Enter' });
  await screen.findByText('Saved.');
  expect(onSave).toHaveBeenCalledTimes(2);
});

it('shows a failed read without guessing a default or enabling writes, then accepts a real read', () => {
  const onRetryLoad = vi.fn();
  const props = pane({ turnDuration: { readRevision: 1, loaded: false, value: undefined, loadError: 'Duration read failed', onRetryLoad, onSave: vi.fn() } });
  expect(screen.queryByRole('textbox', { name: 'Maximum turn duration (minutes)' })).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: /Retry/ }));
  expect(onRetryLoad).toHaveBeenCalledTimes(1);
  props.rerender(<PlannersPane {...props} turnDuration={{ ...props.turnDuration, loaded: true, value: '120', loadError: null }} />);
  expect(screen.getByRole<HTMLInputElement>('textbox', { name: 'Maximum turn duration (minutes)' }).value).toBe('120');
});


it('commits canonical minutes on blur and rejects an invalid blur without clamping it', async () => {
  const props = pane();
  const field = screen.getByRole<HTMLInputElement>('textbox', { name: 'Maximum turn duration (minutes)' });
  fireEvent.change(field, { target: { value: '001' } });
  fireEvent.blur(field);
  expect(props.turnDuration.onSave).toHaveBeenCalledWith('1');
  await screen.findByText('Saved.');
  expect(field.value).toBe('1');
  fireEvent.change(field, { target: { value: '241' } });
  fireEvent.blur(field);
  expect(screen.getByText('Enter a whole number from 1 to 240 minutes (ASCII digits).').textContent).toContain('1 to 240');
  expect(field.value).toBe('241');
  expect(props.turnDuration.onSave).toHaveBeenCalledTimes(1);
});


it('observes a fresh read that restores the pre-save value without retaining an obsolete Saved', async () => {
  const props = pane();
  const field = screen.getByRole<HTMLInputElement>('textbox', { name: 'Maximum turn duration (minutes)' });
  fireEvent.change(field, { target: { value: '45' } });
  fireEvent.keyDown(field, { key: 'Enter' });
  await screen.findByText('Saved.');
  props.rerender(<PlannersPane {...props} turnDuration={{ ...props.turnDuration, readRevision: 2 }} />);
  expect(field.value).toBe('45');
  expect(screen.queryByText('Saved.')).toBeNull();
  expect(screen.getByText(/Changed elsewhere/)).toBeTruthy();
  expect(props.turnDuration.onSave).toHaveBeenCalledTimes(1);
});

it('keeps a contradictory refresh visible when the pending PUT later acknowledges, and permits an explicit new intent', async () => {
  let finish!: () => void;
  const pending = new Promise<void>((resolve) => { finish = resolve; });
  const onSave = vi.fn().mockReturnValueOnce(pending).mockResolvedValue(undefined);
  const props = pane({ turnDuration: { readRevision: 1, value: undefined, loaded: true, loadError: null, onRetryLoad: vi.fn(), onSave } });
  const field = screen.getByRole<HTMLInputElement>('textbox', { name: 'Maximum turn duration (minutes)' });
  fireEvent.change(field, { target: { value: '45' } });
  fireEvent.keyDown(field, { key: 'Enter' });
  props.rerender(<PlannersPane {...props} turnDuration={{ ...props.turnDuration, readRevision: 2 }} />);
  await act(async () => { finish(); await pending; });
  expect(screen.queryByText('Saved.')).toBeNull();
  expect(screen.getByText(/Changed elsewhere/)).toBeTruthy();
  fireEvent.keyDown(field, { key: 'Enter' });
  await screen.findByText('Saved.');
  expect(onSave.mock.calls).toEqual([['45'], ['45']]);
});

it('keeps an unconfirmed reset visible through an unchanged successful read', async () => {
  const props = pane({ turnDuration: { readRevision: 1, value: '30', loaded: true, loadError: null,
    onRetryLoad: vi.fn(), onSave: vi.fn().mockRejectedValue(new Error('lost')) } });
  fireEvent.click(screen.getByRole('button', { name: 'Restore default duration' }));
  await within(row('Maximum turn duration')).findByRole('alert');
  props.rerender(<PlannersPane {...props} turnDuration={{ ...props.turnDuration, readRevision: 2 }} />);
  expect(within(row('Maximum turn duration')).getByRole('alert').textContent).toContain('The save is unconfirmed.');
  expect(props.turnDuration.onSave).toHaveBeenCalledTimes(1);
});

it.each([{ isComposing: true }, { keyCode: 229 }])('does not save composing duration Enter %j', (ime) => {
  const p = pane();
  const field = screen.getByRole('textbox', { name: 'Maximum turn duration (minutes)' });
  fireEvent.change(field, { target: { value: '45' } });
  fireEvent.keyDown(field, { key: 'Enter', ...ime });
  expect(p.turnDuration.onSave).not.toHaveBeenCalled();
  fireEvent.keyDown(field, { key: 'Enter' });
  expect(p.turnDuration.onSave).toHaveBeenCalledWith('45');
});
