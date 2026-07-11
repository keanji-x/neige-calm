// NewTaskForm issue-dev variant tests — issue #891 slice ③ (design §5③).
//
// Surface: the `variant="issue-dev"` flavor of NewTaskForm — URL-driven
// title prefill, disable-on-invalid gating, the exact `workflow_input`
// submit body (pinned), the raw-JSON escape hatch, server-400
// surfacing, and the guarantee that the plain 'task' variant's body is
// unchanged (no workflow keys at all). Same harness style as
// NewTaskForm.test.tsx: api module mocked wholesale, retries disabled,
// fake timers for the cwd-resolve debounce.

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import * as api from '../../api/calm';
import { CalmApiError } from '../../api/calm';
import { DARK_THEME_RGB } from '../../api/themeRgb';
import { NewTaskForm } from './NewTaskForm';

const ISSUE_URL = 'https://github.com/keanji-x/neige-calm/issues/891';

function renderForm(overrides: Partial<React.ComponentProps<typeof NewTaskForm>> = {}) {
  const onCreated = vi.fn(async () => {});
  const onCancel = vi.fn();
  const qc = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  const utils = render(
    <QueryClientProvider client={qc}>
      <NewTaskForm variant="issue-dev" onCreated={onCreated} onCancel={onCancel} {...overrides} />
    </QueryClientProvider>,
  );
  return { ...utils, onCreated, onCancel, qc };
}

const ATLAS = { id: 'cove-1', name: 'Atlas', color: '#5a9', sort: 0, updated_at: 0, created_at: 0 };

function mockCreatedWave() {
  return vi.spyOn(api, 'createWave').mockResolvedValue({
    id: 'w-new',
    cove_id: 'cove-1',
    title: 'dev #891',
    cwd: '/Users/me/code/new',
    lifecycle: 'draft',
    sort: 0,
    archived_at: null,
    terminal_at: null,
    updated_at: 0,
  } as unknown as Awaited<ReturnType<typeof api.createWave>>);
}

/** Drives the shared cwd/cove flow to a submittable state: absolute cwd
 *  typed, resolve misses, defaultCoveId 'cove-1' selected via the
 *  existing-cove branch. Mirrors the plain-variant tests. */
async function fillCwd(user: ReturnType<typeof userEvent.setup>) {
  await user.type(screen.getByLabelText(/working directory/i), '/Users/me/code/new');
  await act(async () => {
    vi.advanceTimersByTime(400);
  });
  await waitFor(() => {
    expect(screen.getByRole('radiogroup', { name: /cove selection/i })).toBeTruthy();
  });
}

beforeEach(() => {
  cleanup();
  document.body.innerHTML = '';
  vi.useFakeTimers({ shouldAdvanceTime: true });
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('NewTaskForm issue-dev — fields + prefill', () => {
  it('renders the issue-dev fields (URL, merge policy, raw JSON) with the variant heading — and NO notes field', () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([]);
    renderForm();
    expect(screen.getByRole('form', { name: /new issue-dev task/i })).toBeTruthy();
    expect(screen.getByLabelText(/github issue url/i)).toBeTruthy();
    expect(screen.getByLabelText(/merge policy/i)).toBeTruthy();
    expect(screen.getByText(/raw workflow_input json/i)).toBeTruthy();
    // #891 signoff: no notes textarea — it duplicated the
    // task-description free-text. notes stays schema-only; the raw-JSON
    // escape hatch is the way to send one.
    expect(screen.queryByLabelText(/notes/i)).toBeNull();
    // The plain fields are still here — cwd flow is reused untouched.
    expect(screen.getByLabelText(/working directory/i)).toBeTruthy();
  });

  it('does NOT render issue-dev fields in the default task variant', () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([]);
    const qc = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });
    render(
      <QueryClientProvider client={qc}>
        <NewTaskForm onCreated={vi.fn()} onCancel={vi.fn()} />
      </QueryClientProvider>,
    );
    expect(screen.getByRole('form', { name: /new task/i })).toBeTruthy();
    expect(screen.queryByLabelText(/github issue url/i)).toBeNull();
    expect(screen.queryByLabelText(/merge policy/i)).toBeNull();
    expect(screen.queryByText(/raw workflow_input json/i)).toBeNull();
  });

  it('prefills the title as dev #<n> once the URL parses, and defaults merge policy to hold-for-ratify', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([]);
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm();
    await user.type(screen.getByLabelText(/github issue url/i), ISSUE_URL);
    const title = screen.getByLabelText(/task description/i) as HTMLTextAreaElement;
    await waitFor(() => expect(title.value).toBe('dev #891'));
    const policy = screen.getByLabelText(/merge policy/i) as HTMLSelectElement;
    expect(policy.value).toBe('hold-for-ratify');
  });

  it('stops prefilling after the user edits the title (latch)', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([]);
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm();
    const urlInput = screen.getByLabelText(/github issue url/i);
    const title = screen.getByLabelText(/task description/i) as HTMLTextAreaElement;
    await user.type(urlInput, ISSUE_URL);
    await waitFor(() => expect(title.value).toBe('dev #891'));
    // Manual edit latches the title…
    await user.clear(title);
    await user.type(title, 'my own title');
    // …so re-pointing the URL at a different issue must not clobber it.
    await user.clear(urlInput);
    await user.type(urlInput, 'https://github.com/o/r/issues/7');
    expect(title.value).toBe('my own title');
  });
});

describe('NewTaskForm issue-dev — validation gating', () => {
  it('disables submit while the URL is empty even with a valid cwd + cove', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([ATLAS]);
    vi.spyOn(api, 'resolveCovePath').mockResolvedValue(null);
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm({ defaultCoveId: 'cove-1' });
    await fillCwd(user);
    expect(screen.getByRole('button', { name: /create task/i })).toBeDisabled();
  });

  it('shows an inline error and disables submit for a malformed URL', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([ATLAS]);
    vi.spyOn(api, 'resolveCovePath').mockResolvedValue(null);
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm({ defaultCoveId: 'cove-1' });
    await fillCwd(user);
    await user.type(
      screen.getByLabelText(/github issue url/i),
      'https://github.com/o/r/pull/42',
    );
    expect(screen.getByText(/must be a github issue url/i)).toBeTruthy();
    expect(screen.getByRole('button', { name: /create task/i })).toBeDisabled();
    // Fixing the URL clears the error and unblocks submit.
    await user.clear(screen.getByLabelText(/github issue url/i));
    await user.type(screen.getByLabelText(/github issue url/i), ISSUE_URL);
    expect(screen.queryByText(/must be a github issue url/i)).toBeNull();
    expect(screen.getByRole('button', { name: /create task/i })).toBeEnabled();
  });
});

describe('NewTaskForm issue-dev — submit body', () => {
  it('pins the exact create body: workflow_id + derived workflow_input, no notes key ever', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([ATLAS]);
    vi.spyOn(api, 'resolveCovePath').mockResolvedValue(null);
    const createSpy = mockCreatedWave();
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    const { onCreated } = renderForm({ defaultCoveId: 'cove-1' });

    await user.type(screen.getByLabelText(/github issue url/i), ISSUE_URL);
    await fillCwd(user);
    await user.click(screen.getByRole('button', { name: /create task/i }));
    await waitFor(() => expect(createSpy).toHaveBeenCalled());

    // The exact wire shape, pinned with toEqual (no extra keys, no
    // missing keys). merge_policy is always sent; notes is NEVER
    // emitted by the form (#891 signoff dropped the field) — only the
    // raw-JSON escape hatch can carry it.
    expect(createSpy.mock.calls[0][0]).toEqual({
      cove_id: 'cove-1',
      title: 'dev #891',
      cwd: '/Users/me/code/new',
      attach_folder: true,
      theme: DARK_THEME_RGB,
      workflow_id: 'issue-development',
      workflow_input: {
        issue_url: 'https://github.com/keanji-x/neige-calm/issues/891',
        repo: 'keanji-x/neige-calm',
        issue_number: 891,
        merge_policy: 'hold-for-ratify',
      },
    });
    await waitFor(() => expect(onCreated).toHaveBeenCalled());
  });

  it('sends the selected merge policy — still with no notes key', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([ATLAS]);
    vi.spyOn(api, 'resolveCovePath').mockResolvedValue(null);
    const createSpy = mockCreatedWave();
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm({ defaultCoveId: 'cove-1' });

    await user.type(screen.getByLabelText(/github issue url/i), ISSUE_URL);
    await user.selectOptions(screen.getByLabelText(/merge policy/i), 'auto-merge');
    await fillCwd(user);
    await user.click(screen.getByRole('button', { name: /create task/i }));
    await waitFor(() => expect(createSpy).toHaveBeenCalled());

    expect(createSpy.mock.calls[0][0].workflow_input).toEqual({
      issue_url: ISSUE_URL,
      repo: 'keanji-x/neige-calm',
      issue_number: 891,
      merge_policy: 'auto-merge',
    });
  });

  it('plain task variant sends no workflow keys at all (byte-identical body)', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([ATLAS]);
    vi.spyOn(api, 'resolveCovePath').mockResolvedValue(null);
    const createSpy = mockCreatedWave();
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    const qc = new QueryClient({
      defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
    });
    render(
      <QueryClientProvider client={qc}>
        <NewTaskForm defaultCoveId="cove-1" onCreated={vi.fn()} onCancel={vi.fn()} />
      </QueryClientProvider>,
    );
    await user.type(screen.getByLabelText(/task description/i), 'do the thing');
    await fillCwd(user);
    await user.click(screen.getByRole('button', { name: /create task/i }));
    await waitFor(() => expect(createSpy).toHaveBeenCalled());
    expect(createSpy.mock.calls[0][0]).toEqual({
      cove_id: 'cove-1',
      title: 'do the thing',
      cwd: '/Users/me/code/new',
      attach_folder: true,
      theme: DARK_THEME_RGB,
    });
  });
});

describe('NewTaskForm issue-dev — raw JSON escape hatch', () => {
  it('prefills the textarea from the parsed fields and live-follows them until edited', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([]);
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm();
    await user.type(screen.getByLabelText(/github issue url/i), ISSUE_URL);
    const ta = screen.getByLabelText(/raw workflow_input json/i) as HTMLTextAreaElement;
    expect(JSON.parse(ta.value)).toEqual({
      issue_url: ISSUE_URL,
      repo: 'keanji-x/neige-calm',
      issue_number: 891,
      merge_policy: 'hold-for-ratify',
    });
    // Still derived: flipping the merge policy updates the mirror.
    await user.selectOptions(screen.getByLabelText(/merge policy/i), 'auto-merge');
    expect(JSON.parse(ta.value).merge_policy).toBe('auto-merge');
  });

  it('an edited raw JSON overrides the derived fields in the submit body — including a notes key, which raw JSON alone can carry (#891 signoff)', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([ATLAS]);
    vi.spyOn(api, 'resolveCovePath').mockResolvedValue(null);
    const createSpy = mockCreatedWave();
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm({ defaultCoveId: 'cove-1' });

    await user.type(screen.getByLabelText(/github issue url/i), ISSUE_URL);
    await fillCwd(user);
    const ta = screen.getByLabelText(/raw workflow_input json/i) as HTMLTextAreaElement;
    const raw = {
      issue_url: 'https://github.com/o/r/issues/7',
      repo: 'o/r',
      issue_number: 7,
      merge_policy: 'auto-merge',
      notes: 'raw mode',
    };
    await user.clear(ta);
    await user.click(ta);
    await user.paste(JSON.stringify(raw));
    // The hint confirms the override is active.
    expect(screen.getByText(/raw json overrides the fields above/i)).toBeTruthy();

    await user.click(screen.getByRole('button', { name: /create task/i }));
    await waitFor(() => expect(createSpy).toHaveBeenCalled());
    expect(createSpy.mock.calls[0][0].workflow_input).toEqual(raw);
    // workflow_id stays hardcoded even under raw mode (F5).
    expect(createSpy.mock.calls[0][0].workflow_id).toBe('issue-development');
  });

  it('invalid raw JSON shows an inline error (wired via aria-describedby) and disables submit; reset works directly from the malformed state', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([ATLAS]);
    vi.spyOn(api, 'resolveCovePath').mockResolvedValue(null);
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm({ defaultCoveId: 'cove-1' });

    await user.type(screen.getByLabelText(/github issue url/i), ISSUE_URL);
    await fillCwd(user);
    expect(screen.getByRole('button', { name: /create task/i })).toBeEnabled();

    const ta = screen.getByLabelText(/raw workflow_input json/i) as HTMLTextAreaElement;
    await user.clear(ta);
    await user.click(ta);
    await user.paste('{"broken":');
    // The parse error is a plain paragraph (role="alert" stays reserved
    // for the submit/server error surface) referenced as the textarea's
    // accessible description while invalid.
    const err = screen.getByText(/invalid json/i);
    expect(ta.getAttribute('aria-invalid')).toBe('true');
    expect(err.id).toBeTruthy();
    expect(ta.getAttribute('aria-describedby')).toBe(err.id);
    expect(screen.getByRole('button', { name: /create task/i })).toBeDisabled();

    // Reset is reachable straight from the malformed state — the user
    // must never have to hand-repair broken JSON to get back to the
    // derived form values.
    await user.click(screen.getByRole('button', { name: /reset to form values/i }));
    expect(JSON.parse(ta.value).issue_number).toBe(891);
    expect(screen.queryByText(/invalid json/i)).toBeNull();
    expect(ta.getAttribute('aria-describedby')).toBeNull();
    expect(ta.getAttribute('aria-invalid')).toBe('false');
    expect(screen.getByRole('button', { name: /create task/i })).toBeEnabled();
  });

  it('reset is also reachable from an emptied textarea (empty string is raw mode, not "no override")', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([ATLAS]);
    vi.spyOn(api, 'resolveCovePath').mockResolvedValue(null);
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm({ defaultCoveId: 'cove-1' });

    await user.type(screen.getByLabelText(/github issue url/i), ISSUE_URL);
    await fillCwd(user);

    const ta = screen.getByLabelText(/raw workflow_input json/i) as HTMLTextAreaElement;
    await user.clear(ta);
    // '' does not parse — submit is gated, but the way out is one click.
    expect(screen.getByText(/invalid json/i)).toBeTruthy();
    expect(screen.getByRole('button', { name: /create task/i })).toBeDisabled();
    await user.click(screen.getByRole('button', { name: /reset to form values/i }));
    expect(JSON.parse(ta.value).issue_number).toBe(891);
    expect(screen.getByRole('button', { name: /create task/i })).toBeEnabled();
  });

  it('a stale raw override outlives later form edits: the summary flags it and the OLD blob ships', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([ATLAS]);
    vi.spyOn(api, 'resolveCovePath').mockResolvedValue(null);
    const createSpy = mockCreatedWave();
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm({ defaultCoveId: 'cove-1' });

    const urlInput = screen.getByLabelText(/github issue url/i);
    await user.type(urlInput, ISSUE_URL);
    await fillCwd(user);
    // Pre-override, the always-visible summary carries no override flag.
    expect(screen.queryByText(/overriding form fields/i)).toBeNull();

    const ta = screen.getByLabelText(/raw workflow_input json/i) as HTMLTextAreaElement;
    const raw = {
      issue_url: 'https://github.com/o/r/issues/7',
      repo: 'o/r',
      issue_number: 7,
      merge_policy: 'auto-merge',
    };
    await user.clear(ta);
    await user.click(ta);
    await user.paste(JSON.stringify(raw));
    // The override indicator lives on the <summary>, which stays visible
    // even when the <details> is collapsed — a stale raw blob must never
    // ship silently.
    expect(
      screen.getByText(/raw workflow_input json — overriding form fields/i),
    ).toBeTruthy();

    // Now edit the URL field. Raw-wins is design-sanctioned: the derived
    // input changes underneath, but the OLD raw blob is what ships.
    await user.clear(urlInput);
    await user.type(urlInput, 'https://github.com/other/repo/issues/999');
    await user.click(screen.getByRole('button', { name: /create task/i }));
    await waitFor(() => expect(createSpy).toHaveBeenCalled());
    expect(createSpy.mock.calls[0][0].workflow_input).toEqual(raw);
  });
});

describe('NewTaskForm issue-dev — server 400 surfacing', () => {
  it('renders the server 400 message (schema validation stays server-side)', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([ATLAS]);
    vi.spyOn(api, 'resolveCovePath').mockResolvedValue(null);
    vi.spyOn(api, 'createWave').mockRejectedValue(
      new CalmApiError(
        400,
        'bad_request',
        'workflow_input.merge_policy: expected one of ["hold-for-ratify","auto-merge"]',
      ),
    );
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm({ defaultCoveId: 'cove-1' });

    await user.type(screen.getByLabelText(/github issue url/i), ISSUE_URL);
    await fillCwd(user);
    await user.click(screen.getByRole('button', { name: /create task/i }));

    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toMatch(
        /workflow_input\.merge_policy: expected one of/i,
      );
    });
  });
});
