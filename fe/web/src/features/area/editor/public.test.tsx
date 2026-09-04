// @vitest-environment jsdom

import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { Dialog } from '../../../ui/dialog/public.tsx';
import { AreaEditorForm, type AreaEditorFormProps } from './public.tsx';

afterEach(cleanup);

const templates = [
  { id: 'small-change', title: 'Small change', tasks: [] },
  { id: 'investigation', title: 'Investigation', tasks: [] },
];

function renderForm(overrides: Partial<AreaEditorFormProps> = {}) {
  const onSubmit = vi.fn();
  const props: AreaEditorFormProps = {
    initial: { name: 'Work', defaultTemplateId: 'small-change', defaultCwd: '/srv/work' },
    submitting: false,
    error: null,
    templates,
    templatesLoaded: true,
    templatesError: null,
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
      <Dialog open title="Edit Work" onClose={props.onCancel} initialFocusRef={props.nameInputRef}>
        <AreaEditorForm {...props} />
      </Dialog>,
    ),
  };
}

describe('AreaEditorForm', () => {
  it('seeds all three values and can explicitly restore both Track defaults', async () => {
    const { onSubmit } = renderForm();
    expect(screen.getByRole<HTMLInputElement>('textbox', { name: /^Name/ }).value).toBe('Work');
    expect(screen.getByRole('button', { name: 'Default template: Small change' }).textContent)
      .toContain('Small change');
    expect(screen.getByRole('button', { name: 'Default folder: /srv/work' }).textContent).toContain('work');
    expect(screen.queryByRole('combobox')).toBeNull();

    await userEvent.click(screen.getByRole('button', { name: 'Default template: Small change' }));
    await userEvent.click(screen.getByRole('menuitem', { name: /^No template/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Use a new Neige workspace' }));
    await userEvent.click(screen.getByRole('button', { name: 'Save changes' }));

    expect(onSubmit).toHaveBeenCalledWith({
      name: 'Work', defaultTemplateId: null, defaultCwd: null,
    });
  });

  it('keeps an unavailable stored template visible instead of silently clearing it', () => {
    renderForm({
      initial: { name: 'Work', defaultTemplateId: 'retired-template', defaultCwd: null },
      templates: [],
    });
    expect(screen.getByRole('button', { name: 'Default template: retired-template (unavailable)' }).textContent)
      .toContain('retired-template (unavailable)');
    expect(screen.getByText('This saved template is not available in this build.')).toBeTruthy();
  });

  it('states an explicit clear honestly when the template read failed', async () => {
    renderForm({ templates: [], templatesLoaded: false, templatesError: 'Could not load templates.' });
    await userEvent.click(screen.getByRole('button', { name: 'Default template: small-change' }));
    await userEvent.click(screen.getByRole('menuitem', { name: /^No template/ }));
    expect(screen.getByText(/Saving now will clear the Area’s default template/)).toBeTruthy();
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

  it('uses Escape to close only the template menu while keeping the Area dialog', async () => {
    renderForm();
    const trigger = screen.getByRole('button', { name: 'Default template: Small change' });
    await userEvent.click(trigger);
    expect(screen.getByRole('menu', { name: 'Default template: Small change' })).toBeTruthy();
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('menu')).toBeNull();
    expect(screen.getByRole('dialog', { name: 'Edit Work' })).toBeTruthy();
    await waitFor(() => expect(document.activeElement).toBe(trigger));
  });

  it('renders a save failure in the form', () => {
    renderForm({ error: 'Could not save the Area.' });
    expect(screen.getByRole('alert').textContent).toContain('Could not save the Area.');
  });

  it('preserves legal leading and trailing spaces in an attached path', async () => {
    const onSubmit = vi.fn();
    renderForm({
      initial: { name: 'Work', defaultTemplateId: null, defaultCwd: '/srv/ work ' },
      onSubmit,
    });
    await userEvent.click(screen.getByRole('button', { name: 'Save changes' }));
    expect(onSubmit).toHaveBeenCalledWith({
      name: 'Work', defaultTemplateId: null, defaultCwd: '/srv/ work ',
    });
  });
});
