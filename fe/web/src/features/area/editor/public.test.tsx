// @vitest-environment jsdom

import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { Dialog } from '../../../ui/dialog/public.tsx';
import { AreaEditorForm, type AreaEditorFormProps } from './public.tsx';

afterEach(cleanup);

function renderForm(overrides: Partial<AreaEditorFormProps> = {}) {
  const onSubmit = vi.fn();
  const props: AreaEditorFormProps = {
    initial: { name: 'Work', defaultCwd: '/srv/work' },
    submitting: false,
    locked: false,
    cancelLabel: 'Cancel',
    error: null,
    listDirectory: vi.fn((path?: string) =>
      Promise.resolve({ path: path ?? '/', parent: null, entries: [] })),
    nameInputRef: { current: null },
    submitLabel: 'Save changes',
    onCancel: vi.fn(),
    onSubmit,
    ...overrides,
  };
  return {
    onSubmit,
    ...render(
      <Dialog purpose="form" origin={{ kind: 'surface-center' }} open title="Edit Work" onClose={props.onCancel} initialFocusRef={props.nameInputRef}>
        <AreaEditorForm {...props} />
      </Dialog>,
    ),
  };
}

describe('AreaEditorForm', () => {
  it('creates without template inputs and preserves a chosen directory regardless of action copy', async () => {
    const onSubmit = vi.fn();
    render(
      <Dialog purpose="form" origin={{ kind: 'surface-center' }} open title="New area" onClose={vi.fn()}>
        <AreaEditorForm
          initial={{ name: 'Work', defaultCwd: null }}
          submitting={false} locked={false} error={null}
          cancelLabel="Cancel" submitLabel="Save changes"
          listDirectory={() => Promise.resolve({ path: '/srv/ legal-space ', parent: null, entries: [] })}
          nameInputRef={{ current: null }} onCancel={vi.fn()} onSubmit={onSubmit}
        />
      </Dialog>,
    );
    expect(screen.queryByRole('button', { name: /template|isolation/i })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Default folder: Neige workspace' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Select this directory' }));
    await userEvent.click(screen.getByRole('button', { name: 'Save changes' }));
    expect(onSubmit).toHaveBeenCalledWith({
      name: 'Work', defaultCwd: '/srv/ legal-space ',
    });
  });

  it('pushes the directory browser into its host Dialog instead of nesting a modal', async () => {
    renderForm();
    const opener = screen.getByRole('button', { name: 'Default folder: /srv/work' });
    await userEvent.click(opener);
    expect(screen.getAllByRole('dialog')).toHaveLength(1);
    expect(screen.getByRole('dialog', { name: 'Choose a directory' })).toBeTruthy();
    const path = await screen.findByRole('combobox', { name: 'Directory path' });
    await waitFor(() => expect(document.activeElement).toBe(path));
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.getByRole('dialog', { name: 'Edit Work' })).toBeTruthy();
    await waitFor(() => expect(document.activeElement).toBe(opener));
  });

  it('renders a save failure in the form', () => {
    renderForm({ error: 'Could not save the Area.' });
    expect(screen.getByRole('alert').textContent).toContain('Could not save the Area.');
  });

  it('preserves legal leading and trailing spaces in an attached path', async () => {
    const onSubmit = vi.fn();
    renderForm({
      initial: { name: 'Work', defaultCwd: '/srv/ work ' },
      onSubmit,
    });
    await userEvent.click(screen.getByRole('button', { name: 'Save changes' }));
    expect(onSubmit).toHaveBeenCalledWith({
      name: 'Work', defaultCwd: '/srv/ work ',
    });
  });
});
