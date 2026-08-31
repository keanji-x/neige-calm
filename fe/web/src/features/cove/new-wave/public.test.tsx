// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { DirectoryListing } from '../../../ui/directory-browser/public.tsx';
import { NewWaveForm } from './public.tsx';

afterEach(cleanup);

const LISTING: DirectoryListing = {
  path: '/srv/app',
  parent: '/srv',
  entries: [{ name: 'crates', path: '/srv/app/crates', isDirectory: true }],
};

function renderForm(overrides: Partial<Parameters<typeof NewWaveForm>[0]> = {}) {
  const props = {
    submitting: false,
    error: null,
    listDirectory: vi.fn(() => Promise.resolve(LISTING)),
    onCancel: vi.fn(),
    onSubmit: vi.fn(),
    ...overrides,
  };
  return { props, ...render(<NewWaveForm {...props} />) };
}

function submitButton(): HTMLButtonElement {
  return screen.getByRole('button', { name: /Create wave|Creating/ });
}

/** Walks the picker to `/srv/app` and confirms it. */
async function pickTheListedFolder(): Promise<void> {
  await userEvent.click(screen.getByLabelText('Folder'));
  // The browser loads on mount and mirrors the listing into its path input;
  // `Select this directory` only enables once the two agree.
  await screen.findByDisplayValue('/srv/app/');
  await userEvent.click(screen.getByRole('button', { name: 'Select this directory' }));
}

describe('NewWaveForm asks for a task, and optionally a folder', () => {
  it('keeps submit disabled while the task is empty', () => {
    renderForm();
    expect(submitButton().disabled).toBe(true);
  });

  it('enables submit after typing a title — the folder is never required', async () => {
    renderForm();
    await userEvent.type(screen.getByLabelText('Task'), 'Ship the thing');
    expect(submitButton().disabled).toBe(false);
  });

  /*
   * The default. No folder chosen ⇒ the draft has **no `cwd` key at all**, not
   * an empty one: the caller keys the whole managed-vs-attached decision on the
   * key's presence, and `cwd: ''` would take the attached branch with a path
   * that cannot work. `toEqual` is what pins the absence; `toMatchObject` would
   * stay green on an extra key.
   */
  it('submits the trimmed title and no folder key when none was chosen', async () => {
    const { props } = renderForm();
    await userEvent.type(screen.getByLabelText('Task'), '  Ship the thing  ');
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({ title: 'Ship the thing' });
    expect(vi.mocked(props.onSubmit).mock.calls[0]?.[0]).not.toHaveProperty('cwd');
  });

  it('submits the picked absolute path as cwd once a folder is chosen', async () => {
    const { props } = renderForm();
    await userEvent.type(screen.getByLabelText('Task'), 'Ship the thing');
    await pickTheListedFolder();
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({ title: 'Ship the thing', cwd: '/srv/app' });
  });

  /* Create time is the only entry into the attached choice, so the way *back*
     to the default has to exist here too — there is no later screen for it. */
  it('drops back to the managed default when the chosen folder is cleared', async () => {
    const { props } = renderForm();
    await userEvent.type(screen.getByLabelText('Task'), 'Ship the thing');
    await pickTheListedFolder();
    await userEvent.click(screen.getByRole('button', { name: 'Use a Neige workspace instead' }));
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({ title: 'Ship the thing' });
  });

  it('offers no way back before a folder is chosen — there is nothing to clear', () => {
    renderForm();
    expect(screen.queryByRole('button', { name: 'Use a Neige workspace instead' })).toBeNull();
  });

  it('reads the directory through the injected port, never a transport of its own', async () => {
    const { props } = renderForm();
    await userEvent.click(screen.getByLabelText('Folder'));
    await screen.findByDisplayValue('/srv/app/');
    expect(props.listDirectory).toHaveBeenCalled();
  });

  /* Cove and claim controls stay cut (#1131): the cove is the opener's, and
     the claim is implied by picking a folder — see `app/shell`. */
  it('asks for no cove and no claim checkbox', async () => {
    renderForm();
    await userEvent.type(screen.getByLabelText('Task'), 'Ship the thing');
    expect(screen.queryByLabelText('Cove')).toBeNull();
    expect(screen.queryByRole('checkbox')).toBeNull();
  });

  /*
   * Single line, not a textarea. The value becomes the wave's `title`, and
   * every other surface renders it as one truncated line — sidebar, wave list,
   * page header — while the wave page edits it through the single-line
   * `EditableTitle`. `getByRole('textbox')` is true of both elements, so this
   * asserts the tag itself; anything weaker would stay green on a textarea.
   */
  it('asks for the task on one line, in the form\'s own sans', () => {
    renderForm();
    const task = screen.getByLabelText('Task');
    expect(task).toHaveProperty('tagName', 'INPUT');
    expect(task).toHaveProperty('type', 'text');
  });

  it('flips the label and blocks submit while submitting', () => {
    renderForm({ submitting: true });
    expect(screen.getByRole('button', { name: 'Creating…' })).toHaveProperty('disabled', true);
  });

  it('surfaces the caller error in an alert region', () => {
    renderForm({ error: 'Could not create the wave.' });
    expect(screen.getByRole('alert').textContent).toContain('Could not create');
  });

  it('cancels without submitting', async () => {
    const { props } = renderForm();
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(props.onCancel).toHaveBeenCalledTimes(1);
    expect(props.onSubmit).not.toHaveBeenCalled();
  });
});
