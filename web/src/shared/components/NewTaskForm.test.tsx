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

  it('shows the conflicting cove name when known to useCovesQuery', async () => {
    // PR3 review followup: when the 409 body's `cove_id` matches a
    // cove we already have in the local cache, the error message
    // should name it ("cove “Atlas”") instead of the generic
    // "another cove" phrasing — so the user can find the offender
    // in the sidebar without copy/pasting a UUID.
    vi.spyOn(api, 'listCoves').mockResolvedValue([
      { id: 'cove-1', name: 'Mine', color: '#5a9', sort: 0, updated_at: 0, created_at: 0 },
      { id: 'cove-other', name: 'Atlas', color: '#c97', sort: 1, updated_at: 0, created_at: 0 },
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
      const txt = screen.getByRole('alert').textContent ?? '';
      expect(txt).toMatch(/Atlas/);
      expect(txt).toMatch(/already claimed/i);
    });
  });
});

describe('NewTaskForm — auto-match override', () => {
  it('lets the user override an auto-matched cove and pick a different one', async () => {
    // PR3 review followup B2: a resolve hit should lock the cove via
    // the auto-match banner BUT also expose an escape hatch ("Use a
    // different cove") so the user isn't trapped if the inferred cove
    // is wrong. Clicking the override reveals the radio picker; a
    // subsequent submit must use the user's manual choice, not the
    // auto-matched id.
    vi.spyOn(api, 'listCoves').mockResolvedValue([
      { id: 'cove-1', name: 'Atlas', color: '#5a9', sort: 0, updated_at: 0, created_at: 0 },
      { id: 'cove-2', name: 'Borealis', color: '#c97', sort: 1, updated_at: 0, created_at: 0 },
    ]);
    vi.spyOn(api, 'resolveCovePath').mockResolvedValue({
      cove_id: 'cove-1',
      folder_id: 1,
      folder_path: '/Users/me/code',
    });
    const createSpy = vi.spyOn(api, 'createWave').mockResolvedValue({
      id: 'w-new',
      cove_id: 'cove-2',
      title: 'do it',
      cwd: '/Users/me/code/proj',
      lifecycle: 'draft',
      sort: 0,
      archived_at: null,
      terminal_at: null,
      updated_at: 0,
    } as unknown as Awaited<ReturnType<typeof api.createWave>>);

    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm();

    await user.type(screen.getByLabelText(/task description/i), 'do it');
    await user.type(screen.getByLabelText(/working directory/i), '/Users/me/code/proj');
    await act(async () => {
      vi.advanceTimersByTime(400);
    });

    // Auto-match banner is visible and the override button is in the
    // tab order with a clear label.
    await waitFor(() => {
      expect(screen.getByTestId('cove-auto-match').textContent).toMatch(/atlas/i);
    });
    const overrideBtn = screen.getByRole('button', { name: /use a different cove/i });
    expect(overrideBtn).toBeTruthy();

    // Click the override — banner collapses, radio picker takes over.
    await user.click(overrideBtn);
    await waitFor(() => {
      expect(screen.queryByTestId('cove-auto-match')).toBeNull();
      expect(screen.getByRole('radiogroup', { name: /cove selection/i })).toBeTruthy();
    });

    // The fallback defaults to the first cove (cove-1). Switch to
    // cove-2 via the select to prove the user can actually pick a
    // different one (not just see the picker).
    const select = screen.getByRole('combobox') as HTMLSelectElement;
    await user.selectOptions(select, 'cove-2');
    await user.click(screen.getByRole('button', { name: /create task/i }));
    await waitFor(() => expect(createSpy).toHaveBeenCalled());
    const body = createSpy.mock.calls[0][0];
    expect(body.cove_id).toBe('cove-2');
    // attach_folder must be true on the override path — we're claiming
    // the cwd under the user's manual cove now, not the auto-matched
    // one that already covers it.
    expect(body.attach_folder).toBe(true);
  });

  it('respects the manual cove choice after a subsequent cwd re-resolves to another hit', async () => {
    // PR3 review followup B2 (latch): once the user overrides, a fresh
    // resolve hit (from re-editing cwd) must NOT auto-overwrite the
    // manual coveChoice — otherwise the override would be silently
    // undone the next time the user keeps typing.
    vi.spyOn(api, 'listCoves').mockResolvedValue([
      { id: 'cove-1', name: 'Atlas', color: '#5a9', sort: 0, updated_at: 0, created_at: 0 },
      { id: 'cove-2', name: 'Borealis', color: '#c97', sort: 1, updated_at: 0, created_at: 0 },
    ]);
    const resolveSpy = vi
      .spyOn(api, 'resolveCovePath')
      .mockResolvedValueOnce({
        cove_id: 'cove-1',
        folder_id: 1,
        folder_path: '/Users/me/code',
      })
      .mockResolvedValueOnce({
        cove_id: 'cove-1',
        folder_id: 1,
        folder_path: '/Users/me/code',
      });
    const createSpy = vi.spyOn(api, 'createWave').mockResolvedValue({
      id: 'w-new',
      cove_id: 'cove-2',
      title: 'x',
      cwd: '/Users/me/code/proj2',
      lifecycle: 'draft',
      sort: 0,
      archived_at: null,
      terminal_at: null,
      updated_at: 0,
    } as unknown as Awaited<ReturnType<typeof api.createWave>>);

    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm();

    await user.type(screen.getByLabelText(/task description/i), 'x');
    const cwdInput = screen.getByLabelText(/working directory/i);
    await user.type(cwdInput, '/Users/me/code/proj1');
    await act(async () => {
      vi.advanceTimersByTime(400);
    });
    await waitFor(() => {
      expect(screen.getByTestId('cove-auto-match')).toBeTruthy();
    });

    // Override → pick cove-2.
    await user.click(screen.getByRole('button', { name: /use a different cove/i }));
    const select = (await screen.findByRole('combobox')) as HTMLSelectElement;
    await user.selectOptions(select, 'cove-2');

    // Now type more into cwd — this triggers a fresh resolve which
    // also hits cove-1. The override must hold; we should still be in
    // the radio picker with cove-2 selected.
    await user.clear(cwdInput);
    await user.type(cwdInput, '/Users/me/code/proj2');
    await act(async () => {
      vi.advanceTimersByTime(400);
    });
    await waitFor(() => expect(resolveSpy).toHaveBeenCalledTimes(2));
    // No auto-match banner — manual choice still owns the UI.
    expect(screen.queryByTestId('cove-auto-match')).toBeNull();

    await user.click(screen.getByRole('button', { name: /create task/i }));
    await waitFor(() => expect(createSpy).toHaveBeenCalled());
    expect(createSpy.mock.calls[0][0].cove_id).toBe('cove-2');
  });
});

describe('NewTaskForm — race guard', () => {
  it('drops a stale resolve when the user has typed past the original cwd', async () => {
    // PR3 review followup N1 (promoted blocker): the resolve effect's
    // `clearTimeout` only kills pending debounces, not in-flight
    // requests. Two overlapping fetches can land out-of-order; without
    // the latest-cwd ref check, the stale one wins and the user's UI
    // shows the wrong cove.
    //
    // Setup: first resolve (for `/a`) takes 100ms and hits cove-1;
    // second resolve (for `/b`) is fast and hits cove-2. Without the
    // race guard, the slow `/a` reply would land last and clobber the
    // `/b` state.
    vi.spyOn(api, 'listCoves').mockResolvedValue([
      { id: 'cove-1', name: 'Atlas', color: '#5a9', sort: 0, updated_at: 0, created_at: 0 },
      { id: 'cove-2', name: 'Borealis', color: '#c97', sort: 1, updated_at: 0, created_at: 0 },
    ]);
    let resolveSlow: ((v: Awaited<ReturnType<typeof api.resolveCovePath>>) => void) | null = null;
    const slowReply = new Promise<Awaited<ReturnType<typeof api.resolveCovePath>>>((res) => {
      resolveSlow = res;
    });
    const spy = vi
      .spyOn(api, 'resolveCovePath')
      .mockImplementationOnce(() => slowReply)
      .mockImplementationOnce(async () => ({
        cove_id: 'cove-2',
        folder_id: 2,
        folder_path: '/b',
      }));

    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    renderForm();

    const cwdInput = screen.getByLabelText(/working directory/i);
    // Type `/a` and flush the debounce → fires the first (slow) fetch.
    await user.type(cwdInput, '/a');
    await act(async () => {
      vi.advanceTimersByTime(400);
    });
    await waitFor(() => expect(spy).toHaveBeenCalledTimes(1));

    // Type `/b` (clear + retype) and flush → fires the second fetch.
    await user.clear(cwdInput);
    await user.type(cwdInput, '/b');
    await act(async () => {
      vi.advanceTimersByTime(400);
    });
    await waitFor(() => expect(spy).toHaveBeenCalledTimes(2));

    // The second (fast) fetch lands → auto-match banner for cove-2.
    await waitFor(() => {
      expect(screen.getByTestId('cove-auto-match').textContent).toMatch(/borealis/i);
    });

    // Now let the slow `/a` reply land. With the race guard it must be
    // dropped: the banner must still show Borealis, not Atlas.
    await act(async () => {
      resolveSlow?.({ cove_id: 'cove-1', folder_id: 1, folder_path: '/a' });
      // Let the microtask flush.
      await Promise.resolve();
    });
    expect(screen.getByTestId('cove-auto-match').textContent).toMatch(/borealis/i);
    expect(screen.getByTestId('cove-auto-match').textContent).not.toMatch(/atlas/i);
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
