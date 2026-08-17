// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Cove, CoveFolder } from '../../../../../core/domain/cove.ts';
import { NewWaveForm } from './public.tsx';

afterEach(cleanup);

const COVES: readonly Cove[] = [
  { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0 },
  { id: 'c2', name: 'Home', color: '#8B7FE8', sort: 2, kind: 'user', createdAt: 0, updatedAt: 0 },
];

function folder(id: number, path: string, coveId = 'c1'): CoveFolder {
  return { id, coveId, path, repoIdentity: null, repoIdentityProbedAt: null, createdAt: 0 };
}

function renderForm(overrides: Partial<Parameters<typeof NewWaveForm>[0]> = {}) {
  const props = {
    coves: COVES,
    coveId: 'c1',
    onCoveIdChange: vi.fn(),
    folders: [folder(1, '/srv/work')],
    foldersLoading: false,
    submitting: false,
    error: null,
    onCancel: vi.fn(),
    onSubmit: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<NewWaveForm {...props} />) };
}

function submitButton(): HTMLButtonElement {
  return screen.getByRole('button', { name: /Create wave|Creating/ });
}

/**
 * INV-NEWWAVE-002 — the form's shape is a function of the target cove's folder
 * count, and `attachFolder` is derived from it rather than asked. These three
 * describes are the three counts.
 */
describe('NewWaveForm with exactly one claimed folder', () => {
  it('asks for the task only — no folder control, and no checkbox', async () => {
    renderForm();
    await userEvent.type(screen.getByLabelText('Task'), 'Ship the thing');
    expect(screen.queryByLabelText('Folder')).toBeNull();
    expect(screen.queryByLabelText(/Claim this folder/)).toBeNull();
    expect(screen.queryByRole('checkbox')).toBeNull();
    expect(submitButton().disabled).toBe(false);
  });

  it('sends the folder the cove already owns, and never claims it again', async () => {
    const { props } = renderForm();
    await userEvent.type(screen.getByLabelText('Task'), '  Ship the thing  ');
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({
      coveId: 'c1', title: 'Ship the thing', cwd: '/srv/work', attachFolder: false,
    });
  });

  it('keeps submit disabled while the task is empty', () => {
    renderForm();
    expect(submitButton().disabled).toBe(true);
  });

  /*
   * Single line, not a textarea. The value becomes the wave's `title`, and
   * every other surface renders it as one truncated line — sidebar, wave list,
   * page header — while the wave page edits it through the single-line
   * `EditableTitle`. `getByRole('textbox')` is true of both elements, so this
   * asserts the tag itself; anything weaker would stay green on a textarea.
   *
   * The class matters too: `fe-design.md:869` allows `--font-mono` only for a
   * prompt or a path, so the mono class belongs to the folder input and not to
   * this one.
   */
  it('asks for the task on one line, in the form\'s own sans', () => {
    renderForm({ folders: [] });
    const task = screen.getByLabelText('Task');
    expect(task).toHaveProperty('tagName', 'INPUT');
    expect(task).toHaveProperty('type', 'text');
    expect(task.className).not.toMatch(/pathInput/);
    expect(screen.getByLabelText('Folder').className).toMatch(/pathInput/);
  });
});

describe('NewWaveForm with several claimed folders', () => {
  const folders = [folder(2, '/srv/alpha'), folder(1, '/srv/beta')];

  it('offers the folders as a select, defaulting to the first the caller sorted', () => {
    renderForm({ folders });
    const select = screen.getByLabelText('Folder');
    expect(select).toHaveProperty('value', '/srv/alpha');
    expect([...(select as HTMLSelectElement).options].map((option) => option.value))
      .toEqual(['/srv/alpha', '/srv/beta']);
  });

  it('submits the picked folder with attachFolder false', async () => {
    const { props } = renderForm({ folders });
    await userEvent.type(screen.getByLabelText('Task'), 'Ship it');
    await userEvent.selectOptions(screen.getByLabelText('Folder'), '/srv/beta');
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({
      coveId: 'c1', title: 'Ship it', cwd: '/srv/beta', attachFolder: false,
    });
  });

  /*
   * The selection is derived, not stored in an effect: a pick that no longer
   * exists falls back to the new cove's first folder in the *same* render. An
   * effect would have left one submittable frame carrying the previous cove's
   * path.
   */
  it('falls back to the new cove\'s first folder when the cove changes under it', async () => {
    const { props, rerender } = renderForm({ folders });
    await userEvent.type(screen.getByLabelText('Task'), 'Ship it');
    await userEvent.selectOptions(screen.getByLabelText('Folder'), '/srv/beta');
    rerender(<NewWaveForm {...props} coveId="c2" folders={[folder(9, '/home/notes', 'c2'), folder(8, '/home/x', 'c2')]} />);
    expect(screen.getByLabelText('Folder')).toHaveProperty('value', '/home/notes');
  });
});

describe('NewWaveForm with no claimed folder', () => {
  it('asks for the cove\'s first folder and says that is what it is', () => {
    renderForm({ folders: [] });
    expect(screen.getByLabelText('Folder')).toHaveProperty('tagName', 'INPUT');
    expect(screen.getByText(/no folder yet/i)).toBeTruthy();
  });

  /** INV-NEWWAVE-001 — a relative path would land wherever the server runs. */
  it('keeps submit disabled for a relative path and explains why', async () => {
    renderForm({ folders: [] });
    await userEvent.type(screen.getByLabelText('Task'), 'Ship the thing');
    await userEvent.type(screen.getByLabelText('Folder'), 'tmp/x');
    expect(submitButton().disabled).toBe(true);
    expect(screen.getByText(/absolute path/)).toBeTruthy();
  });

  it('claims the folder without asking, because there is no other legal outcome', async () => {
    const { props } = renderForm({ folders: [] });
    await userEvent.type(screen.getByLabelText('Task'), 'Ship it');
    await userEvent.type(screen.getByLabelText('Folder'), '/tmp/x ');
    expect(screen.queryByRole('checkbox')).toBeNull();
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({
      coveId: 'c1', title: 'Ship it', cwd: '/tmp/x', attachFolder: true,
    });
  });
});

describe('NewWaveForm cove selection and caller state', () => {
  it('shows the cove the caller preselected and reports a change upward', async () => {
    const { props } = renderForm({ coveId: 'c2' });
    expect(screen.getByLabelText('Cove')).toHaveProperty('value', 'c2');
    await userEvent.selectOptions(screen.getByLabelText('Cove'), 'c1');
    expect(props.onCoveIdChange).toHaveBeenCalledWith('c1');
  });

  /* The count decides the form's shape *and* `attachFolder`, so an unresolved
     read is not "zero folders" — submitting on it would claim a folder for a
     cove that may already own one. */
  it('blocks submit while the folder read is still in flight', async () => {
    renderForm({ folders: [], foldersLoading: true });
    await userEvent.type(screen.getByLabelText('Task'), 'Ship it');
    expect(screen.queryByLabelText('Folder')).toBeNull();
    expect(submitButton().disabled).toBe(true);
  });

  it('flips the label and blocks submit while submitting', () => {
    renderForm({ submitting: true });
    expect(screen.getByRole('button', { name: 'Creating…' })).toHaveProperty('disabled', true);
  });

  it('surfaces the caller error in an alert region', () => {
    renderForm({ error: 'Directory already claimed by cove Home' });
    expect(screen.getByRole('alert').textContent).toContain('already claimed');
  });

  it('cancels without submitting', async () => {
    const { props } = renderForm();
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(props.onCancel).toHaveBeenCalledTimes(1);
    expect(props.onSubmit).not.toHaveBeenCalled();
  });
});
