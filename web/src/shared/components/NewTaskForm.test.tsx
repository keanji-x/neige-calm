// NewTaskForm unit tests — issue #250 PR 3.
//
// Surface: the form's cwd → resolve → cove-inference flow, inline cwd
// validation, the two submit branches (existing cove + new cove), and
// the structured 409 (FolderConflict) error rendering. We mock the
// `api` module wholesale because the form drives real network shape
// via TanStack Query mutations; the QueryClientProvider here uses
// retries disabled so a thrown promise surfaces synchronously to the
// test instead of waiting through retry backoff.

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import * as api from '../../api/calm';
import { CalmApiError } from '../../api/calm';
import { NewTaskForm } from './NewTaskForm';

function renderForm(overrides: Partial<React.ComponentProps<typeof NewTaskForm>> = {}) {
  const onCreated = vi.fn(async () => {});
  const onCancel = vi.fn();
  const qc = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  const utils = render(
    <QueryClientProvider client={qc}>
      <NewTaskForm onCreated={onCreated} onCancel={onCancel} {...overrides} />
    </QueryClientProvider>,
  );
  return { ...utils, onCreated, onCancel, qc };
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

describe('NewTaskForm — initial render', () => {
  it('renders title, cwd, and cove fields with required labels', () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([]);
    renderForm();
    expect(screen.getByLabelText(/task description/i)).toBeTruthy();
    expect(screen.getByLabelText(/working directory/i)).toBeTruthy();
    // Heading is "New task"; the wrapping section is role=form for
    // a11y lookup.
    expect(screen.getByRole('form', { name: /new task/i })).toBeTruthy();
  });

  it('disables Create when no title/cwd entered', () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([]);
    renderForm();
    expect(screen.getByRole('button', { name: /create task/i })).toBeDisabled();
  });
});

describe('NewTaskForm — cwd validation + cove inference', () => {
  it('shows inline error for non-absolute cwd', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([]);
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm();
    const cwd = screen.getByLabelText(/working directory/i);
    await user.type(cwd, 'relative/path');
    expect(screen.getByText(/must be absolute/i)).toBeTruthy();
    expect(screen.getByRole('button', { name: /create task/i })).toBeDisabled();
  });

  it('calls resolveCovePath when an absolute path is entered (debounced)', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([]);
    const spy = vi.spyOn(api, 'resolveCovePath').mockResolvedValue(null);
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm();
    const cwd = screen.getByLabelText(/working directory/i);
    await user.type(cwd, '/Users/me/code/project');
    // No call before the debounce window closes.
    expect(spy).not.toHaveBeenCalled();
    await act(async () => {
      vi.advanceTimersByTime(400);
    });
    await waitFor(() => expect(spy).toHaveBeenCalledWith('/Users/me/code/project'));
  });

  it('renders auto-match banner when resolve hits', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([
      { id: 'cove-1', name: 'Atlas', color: '#5a9', sort: 0, updated_at: 0, created_at: 0 },
    ]);
    vi.spyOn(api, 'resolveCovePath').mockResolvedValue({
      cove_id: 'cove-1',
      folder_id: 1,
      folder_path: '/Users/me/code',
    });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm();
    await user.type(screen.getByLabelText(/working directory/i), '/Users/me/code/proj');
    await act(async () => {
      vi.advanceTimersByTime(400);
    });
    await waitFor(() => {
      expect(screen.getByTestId('cove-auto-match').textContent).toMatch(/atlas/i);
    });
    // The radiogroup should NOT render in auto mode — cove is locked.
    expect(screen.queryByRole('radiogroup', { name: /cove selection/i })).toBeNull();
  });

  it('renders cove dropdown / new-cove input when resolve misses', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([
      { id: 'cove-1', name: 'Atlas', color: '#5a9', sort: 0, updated_at: 0, created_at: 0 },
    ]);
    vi.spyOn(api, 'resolveCovePath').mockResolvedValue(null);
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm();
    await user.type(screen.getByLabelText(/working directory/i), '/Users/me/code/new');
    await act(async () => {
      vi.advanceTimersByTime(400);
    });
    await waitFor(() => {
      expect(screen.getByRole('radiogroup', { name: /cove selection/i })).toBeTruthy();
    });
    // Existing-cove + new-cove radios are present.
    expect(screen.getByLabelText(/existing cove/i)).toBeTruthy();
    expect(screen.getByLabelText(/create new cove/i)).toBeTruthy();
  });
});

describe('NewTaskForm — submit', () => {
  it('posts createWave with attach_folder=true for the existing-cove branch', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([
      { id: 'cove-1', name: 'Atlas', color: '#5a9', sort: 0, updated_at: 0, created_at: 0 },
    ]);
    vi.spyOn(api, 'resolveCovePath').mockResolvedValue(null);
    const createSpy = vi.spyOn(api, 'createWave').mockResolvedValue({
      id: 'w-new',
      cove_id: 'cove-1',
      title: 'do the thing',
      cwd: '/Users/me/code/new',
      lifecycle: 'draft',
      sort: 0,
      archived_at: null,
      terminal_at: null,
      updated_at: 0,
    } as unknown as Awaited<ReturnType<typeof api.createWave>>);

    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    const { onCreated } = renderForm({ defaultCoveId: 'cove-1' });

    await user.type(screen.getByLabelText(/task description/i), 'do the thing');
    await user.type(screen.getByLabelText(/working directory/i), '/Users/me/code/new');
    await act(async () => {
      vi.advanceTimersByTime(400);
    });
    await waitFor(() => {
      expect(screen.getByRole('radiogroup', { name: /cove selection/i })).toBeTruthy();
    });
    // Default mode under defaultCoveId is "existing" — submit straight away.
    await user.click(screen.getByRole('button', { name: /create task/i }));
    await waitFor(() => expect(createSpy).toHaveBeenCalled());
    const body = createSpy.mock.calls[0][0];
    expect(body.cove_id).toBe('cove-1');
    expect(body.cwd).toBe('/Users/me/code/new');
    expect(body.attach_folder).toBe(true);
    expect(body.title).toBe('do the thing');
    expect(body.theme).toMatchObject({ fg: expect.any(Array), bg: expect.any(Array) });
    await waitFor(() => expect(onCreated).toHaveBeenCalled());
  });

  it('mints a new cove first, then posts the wave for the new-cove branch', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([]);
    vi.spyOn(api, 'resolveCovePath').mockResolvedValue(null);
    const newCove = {
      id: 'cove-new',
      name: 'Project Z',
      color: '#5a9',
      sort: 0,
      updated_at: 0,
      created_at: 0,
    };
    const coveSpy = vi.spyOn(api, 'createCove').mockResolvedValue(
      newCove as unknown as Awaited<ReturnType<typeof api.createCove>>,
    );
    const waveSpy = vi.spyOn(api, 'createWave').mockResolvedValue({
      id: 'w-new',
      cove_id: 'cove-new',
      title: 'hi',
      cwd: '/x',
      lifecycle: 'draft',
      sort: 0,
      archived_at: null,
      terminal_at: null,
      updated_at: 0,
    } as unknown as Awaited<ReturnType<typeof api.createWave>>);

    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm();

    await user.type(screen.getByLabelText(/task description/i), 'hi');
    await user.type(screen.getByLabelText(/working directory/i), '/x');
    await act(async () => {
      vi.advanceTimersByTime(400);
    });
    // Default mode (no defaultCoveId) is "new" — fill the new-cove name.
    await waitFor(() => {
      expect(screen.getByLabelText(/new cove name/i)).toBeTruthy();
    });
    await user.type(screen.getByLabelText(/new cove name/i), 'Project Z');
    await user.click(screen.getByRole('button', { name: /create task/i }));
    await waitFor(() => expect(coveSpy).toHaveBeenCalled());
    expect(coveSpy.mock.calls[0][0].name).toBe('Project Z');
    await waitFor(() => expect(waveSpy).toHaveBeenCalled());
    const body = waveSpy.mock.calls[0][0];
    expect(body.cove_id).toBe('cove-new');
    expect(body.attach_folder).toBe(true);
  });

  it('surfaces folder-conflict 409 with a user-readable message', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([
      { id: 'cove-1', name: 'Atlas', color: '#5a9', sort: 0, updated_at: 0, created_at: 0 },
    ]);
    vi.spyOn(api, 'resolveCovePath').mockResolvedValue(null);
    vi.spyOn(api, 'createWave').mockRejectedValue(
      new CalmApiError(409, 'conflict', 'conflict', {
        folder_id: 2,
        cove_id: 'cove-other',
        conflict_path: '/Users/me/code',
        conflict_kind: 'descendant',
      }),
    );

    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm({ defaultCoveId: 'cove-1' });

    await user.type(screen.getByLabelText(/task description/i), 'do');
    await user.type(screen.getByLabelText(/working directory/i), '/Users/me/code/x');
    await act(async () => {
      vi.advanceTimersByTime(400);
    });
    await waitFor(() => {
      expect(screen.getByRole('radiogroup', { name: /cove selection/i })).toBeTruthy();
    });
    await user.click(screen.getByRole('button', { name: /create task/i }));

    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toMatch(/already claimed by another cove/i);
    });
  });
});

describe('NewTaskForm — cancel', () => {
  it('Escape calls onCancel', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([]);
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    const { onCancel } = renderForm();
    await user.type(screen.getByLabelText(/task description/i), 'wip');
    await user.keyboard('{Escape}');
    expect(onCancel).toHaveBeenCalled();
  });

  it('Cancel button calls onCancel', async () => {
    vi.spyOn(api, 'listCoves').mockResolvedValue([]);
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    const { onCancel } = renderForm();
    await user.click(screen.getByRole('button', { name: /cancel/i }));
    expect(onCancel).toHaveBeenCalled();
  });
});
