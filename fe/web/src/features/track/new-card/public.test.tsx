// @vitest-environment jsdom
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { createRef } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { CardAddMenuEntry } from '../../../systems/cards/public.js';
import type { ListDirectory } from '../../../ui/directory-browser/public.tsx';
import { Dialog } from '../../../ui/dialog/public.tsx';
import { AddCardMenu, NewCardForm } from './public.tsx';

afterEach(cleanup);

const TERMINAL: CardAddMenuEntry = Object.freeze({ type: 'terminal', label: 'terminal', fields: [] });
const CODEX: CardAddMenuEntry = Object.freeze({
  type: 'codex',
  label: 'codex',
  fields: Object.freeze([
    Object.freeze({ key: 'title', label: 'Title', kind: 'text' as const }),
    Object.freeze({ key: 'cwd', label: 'Working directory', kind: 'directory' as const }),
  ]),
});

const listDirectory: ListDirectory = () => Promise.resolve({ path: '/', parent: null, entries: [] });

describe('AddCardMenu', () => {
  it('lists one item per registry row and reports the picked entry', async () => {
    const onSelect = vi.fn();
    render(<AddCardMenu entries={[TERMINAL, CODEX]} onSelect={onSelect} />);
    await userEvent.click(screen.getByRole('button', { name: 'Add card' }));
    expect(screen.getAllByRole('menuitem').map((item) => item.textContent)).toEqual(['terminal', 'codex']);
    await userEvent.click(screen.getByRole('menuitem', { name: 'codex' }));
    expect(onSelect).toHaveBeenCalledWith(CODEX);
  });

  it('keeps the trigger and says so when no kind is available', async () => {
    const onSelect = vi.fn();
    render(<AddCardMenu entries={[]} onSelect={onSelect} />);
    await userEvent.click(screen.getByRole('button', { name: 'Add card' }));
    const rows = screen.getAllByRole('menuitem');
    expect(rows.map((row) => row.textContent)).toEqual(['No card kinds available']);
    expect(rows[0]?.getAttribute('aria-disabled')).toBe('true');
    await userEvent.click(screen.getByRole('menuitem', { name: 'No card kinds available' }));
    expect(onSelect).not.toHaveBeenCalled();
  });
});

describe('NewCardForm', () => {
  function renderForm(entry: CardAddMenuEntry, overrides: {
    submitting?: boolean; error?: string | null; onSubmit?: (values: Readonly<Record<string, string>>) => void;
  } = {}) {
    const onSubmit = overrides.onSubmit ?? vi.fn();
    render(
      <NewCardForm
        entry={entry}
        submitting={overrides.submitting ?? false}
        error={overrides.error ?? null}
        listDirectory={listDirectory}
        firstFieldRef={createRef<HTMLInputElement>()}
        onCancel={vi.fn()}
        onSubmit={onSubmit}
      />,
    );
    return onSubmit;
  }

  /* This form renders inside `app/router`'s add-card dialog, so the folder control must push into that dialog through `useDialogView` and never open a second one: a nested dialog fights the outer focus trap. */
  it('pushes the folder picker into the surrounding dialog, opening no second one', async () => {
    render(
      <Dialog open onClose={vi.fn()} title="Add card">
        <NewCardForm
          entry={CODEX}
          submitting={false}
          error={null}
          listDirectory={listDirectory}
          firstFieldRef={createRef<HTMLInputElement>()}
          onCancel={vi.fn()}
          onSubmit={vi.fn()}
        />
      </Dialog>,
    );
    const outer = screen.getByRole('dialog');
    expect(outer.getAttribute('aria-label')).toBe('Add card');

    await userEvent.click(screen.getByRole('button', { name: /Working directory/ }));

    expect(screen.getAllByRole('dialog')).toHaveLength(1);
    expect(screen.getByRole('dialog')).toBe(outer);
    await waitFor(() => expect(outer.getAttribute('aria-label')).toBe('Choose a directory'));
  });

  it('renders the entry\'s declared fields and submits what was typed', async () => {
    const onSubmit = renderForm(CODEX);
    await userEvent.type(screen.getByLabelText('Title'), 'Rewrite the parser');
    await userEvent.click(screen.getByRole('button', { name: 'Create codex' }));
    expect(onSubmit).toHaveBeenCalledWith({ title: 'Rewrite the parser' });
  });

  it('omits a field the reader never touched', async () => {
    const onSubmit = renderForm(CODEX);
    await userEvent.click(screen.getByRole('button', { name: 'Create codex' }));
    expect(onSubmit).toHaveBeenCalledWith({});
  });

  it('blocks the submit while a required field is empty', async () => {
    const required: CardAddMenuEntry = Object.freeze({
      type: 'file-viewer',
      label: 'file',
      fields: Object.freeze([
        Object.freeze({ key: 'path', label: 'File or folder', kind: 'file' as const, required: true }),
      ]),
    });
    const onSubmit = renderForm(required);
    const submit = screen.getByRole('button', { name: 'Create file' });
    expect(submit.hasAttribute('disabled') || submit.getAttribute('aria-disabled') === 'true').toBe(true);
    await userEvent.click(submit);
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it('shows the create failure and keeps the form open to retry', () => {
    renderForm(CODEX, { error: 'track … is not a git repository' });
    expect(screen.getByText('track … is not a git repository')).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Create codex' })).toBeTruthy();
  });

  it('says it is working while the create is in flight', () => {
    renderForm(CODEX, { submitting: true });
    expect(screen.getByRole('button', { name: 'Creating…' })).toBeTruthy();
  });
});
