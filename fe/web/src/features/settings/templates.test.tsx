// @vitest-environment jsdom
// Settings › Templates — the list and the editor (#1230).
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { WaveTemplate } from '../../../../core/domain/wave.ts';
import {
  TemplateEditorPage, TemplateListPage,
  type TemplateEditorProps, type TemplateListProps, type TemplateSave,
} from './templates.tsx';

// astryx's `Spinner` reaches `window.matchMedia` unguarded and jsdom has none.
// Stubbed per-file rather than globally — `app/theme` deliberately branches on
// `matchMedia` being absent, and a global polyfill would hide that path.
beforeEach(() => {
  vi.stubGlobal('matchMedia', vi.fn(() => ({
    matches: false, media: '', onchange: null,
    addEventListener: vi.fn(), removeEventListener: vi.fn(),
    addListener: vi.fn(), removeListener: vi.fn(), dispatchEvent: vi.fn(),
  })));
});

afterEach(cleanup);

const SMALL_CHANGE: WaveTemplate = {
  id: 'small-change',
  title: 'Small change',
  tasks: [
    { key: 'inspect', goal: 'Read the requested change.' },
    { key: 'implement', goal: 'Implement and commit.' },
  ],
};

function listProps(overrides: Partial<TemplateListProps> = {}): TemplateListProps {
  return {
    templates: [SMALL_CHANGE],
    loadError: null,
    onRetryLoad: vi.fn(),
    onOpenSettings: vi.fn(),
    onEdit: vi.fn(),
    ...overrides,
  };
}

function editorProps(overrides: Partial<TemplateEditorProps> = {}): TemplateEditorProps {
  return {
    template: SMALL_CHANGE,
    loadError: null,
    onRetryLoad: vi.fn(),
    saving: false,
    saveError: null,
    savedAt: null,
    onSave: vi.fn(),
    onOpenTemplates: vi.fn(),
    ...overrides,
  };
}

describe('Template list', () => {
  it('names each Edit button after its template', async () => {
    const onEdit = vi.fn();
    render(<TemplateListPage {...listProps({ onEdit })} />);
    // Three buttons all called "Edit" is a list a screen reader cannot use.
    await userEvent.click(screen.getByRole('button', { name: 'Edit Small change' }));
    expect(onEdit).toHaveBeenCalledWith('small-change');
  });

  it('summarises a template by its task count, not by listing every task', () => {
    render(<TemplateListPage {...listProps()} />);
    expect(screen.getByText('2 tasks')).toBeTruthy();
    // The goals belong to the next screen; repeating them here is what made
    // the inline version unreadable.
    expect(screen.queryByText('Read the requested change.')).toBeNull();
  });

  it('says one task in the singular', () => {
    render(<TemplateListPage {...listProps({
      templates: [{ ...SMALL_CHANGE, tasks: [SMALL_CHANGE.tasks[0]] }],
    })} />);
    expect(screen.getByText('1 task')).toBeTruthy();
  });

  it('renders no list at all while the definitions are loading', () => {
    render(<TemplateListPage {...listProps({ templates: undefined })} />);
    expect(screen.queryAllByRole('listitem').length).toBe(0);
    expect(screen.getByText('Loading templates…')).toBeTruthy();
  });

  it('retries a failed read in place', async () => {
    const onRetryLoad = vi.fn();
    render(<TemplateListPage {...listProps({
      templates: undefined, loadError: 'Could not load templates.', onRetryLoad,
    })} />);
    await userEvent.click(screen.getByRole('button', { name: 'Retry' }));
    expect(onRetryLoad).toHaveBeenCalledTimes(1);
  });

  it('leaves for Settings through the breadcrumb callback', async () => {
    const onOpenSettings = vi.fn();
    render(<TemplateListPage {...listProps({ onOpenSettings })} />);
    await userEvent.click(screen.getByRole('button', { name: 'Settings' }));
    expect(onOpenSettings).toHaveBeenCalledTimes(1);
  });
});

describe('Template editor', () => {
  it('labels each goal field with the task key it belongs to', () => {
    render(<TemplateEditorPage {...editorProps()} />);
    expect(screen.getByLabelText<HTMLInputElement>('inspect').value).toBe('Read the requested change.');
    expect(screen.getByLabelText<HTMLInputElement>('implement').value).toBe('Implement and commit.');
  });

  /**
   * The save is a **diff**: only the goals that actually changed, plus the
   * appends. Sending every task would re-assert values nobody edited, which is
   * the defect INV-SETTINGS-001 removes on the settings form; and sending task
   * *objects* is what let round 2's privileged vocabulary through.
   */
  it('sends only the goals that changed, and never a task object', async () => {
    const onSave = vi.fn();
    render(<TemplateEditorPage {...editorProps({ onSave })} />);
    await userEvent.clear(screen.getByLabelText('inspect'));
    await userEvent.type(screen.getByLabelText('inspect'), 'Look first.');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));

    expect(onSave).toHaveBeenCalledTimes(1);
    expect(onSave.mock.calls[0][0]).toEqual({
      id: 'small-change',
      title: 'Small change',
      // `implement` was untouched and must not appear.
      edits: [{ key: 'inspect', goal: 'Look first.' }],
      appends: [],
    });
  });

  it('has no way to express a task field other than key and goal', async () => {
    const onSave = vi.fn();
    render(<TemplateEditorPage {...editorProps({ onSave })} />);
    await userEvent.type(screen.getByLabelText('Title'), '!');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    const sent = onSave.mock.calls[0][0] as TemplateSave;
    for (const entry of [...sent.edits, ...sent.appends]) {
      // Not "these fields are absent" — "these are the ONLY fields". A
      // whitelist check would pass a payload that also carried `spawn`.
      expect(Object.keys(entry).sort()).toEqual(['goal', 'key']);
    }
  });

  it('keeps Save disabled until something changes, and disables it again after Reset', async () => {
    render(<TemplateEditorPage {...editorProps()} />);
    expect(screen.getByRole('button', { name: 'Save' }).hasAttribute('disabled')).toBe(true);
    await userEvent.type(screen.getByLabelText('Title'), '!');
    expect(screen.getByRole('button', { name: 'Save' }).hasAttribute('disabled')).toBe(false);
    await userEvent.click(screen.getByRole('button', { name: 'Reset' }));
    expect(screen.getByLabelText<HTMLInputElement>('Title').value).toBe('Small change');
    expect(screen.getByRole('button', { name: 'Save' }).hasAttribute('disabled')).toBe(true);
  });

  /**
   * #1179 — renaming a key and removing a task are refused by the server, so
   * the affordances must not exist. A control whose only outcome is a 400 is
   * worse than an absent one.
   */
  it('offers no way to rename a key, delete a task, or reorder the list', () => {
    render(<TemplateEditorPage {...editorProps()} />);
    for (const name of [/delete/i, /remove/i, /rename/i, /move up/i, /move down/i]) {
      expect(screen.queryByRole('button', { name })).toBeNull();
    }
    // The keys are shown as *labels*, never as editable fields. The previous
    // spelling — `queryByLabelText('Key')?.getAttribute('value')` — could never
    // fail: that query only ever resolves to NewTaskRow's empty add-field, so
    // the comparison was false by construction. Assert the real property
    // instead: no textbox anywhere on the page holds an existing task key as
    // its value, whatever that field might be labelled.
    const values = screen.getAllByRole('textbox').map((box) => (box as HTMLInputElement).value);
    for (const key of ['inspect', 'implement']) {
      expect(values).not.toContain(key);
    }
    // And the limit is stated on the page rather than discovered by failing.
    expect(screen.getByText(/key is fixed once the template exists/)).toBeTruthy();
  });

  it('appends a task as a bare key/goal pair and clears the add fields', async () => {
    const onSave = vi.fn();
    render(<TemplateEditorPage {...editorProps({ onSave })} />);
    await userEvent.type(screen.getByLabelText('Key'), 'hand-off');
    await userEvent.type(screen.getByLabelText('Goal'), 'Summarize.');
    await userEvent.click(screen.getByRole('button', { name: 'Add task' }));

    expect(screen.getByLabelText<HTMLInputElement>('Key').value).toBe('');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    const sent = onSave.mock.calls[0][0] as TemplateSave;
    expect(sent.appends).toEqual([{ key: 'hand-off', goal: 'Summarize.' }]);
    // The block's `kind` / `no_gate_reason` / `declared_by` are the server's to
    // set — the editor must not be able to state them at all.
    expect(Object.keys(sent.appends[0]).sort()).toEqual(['goal', 'key']);
  });

  /**
   * The client key check mirrors `key_is_valid`, so it must agree in both
   * directions: nothing the server accepts may be blocked here, and nothing the
   * server refuses may be offered.
   */
  it('mirrors key_is_valid in both directions', async () => {
    render(<TemplateEditorPage {...editorProps()} />);
    await userEvent.type(screen.getByLabelText('Goal'), 'Something.');
    const add = () => screen.getByRole('button', { name: 'Add task' });
    const key = () => screen.getByLabelText('Key');

    // Accepted by the server, so they must be offered here.
    for (const valid of ['run.tests', 'run_tests', 'a', '9lives', 'a'.repeat(64)]) {
      await userEvent.clear(key());
      await userEvent.type(key(), valid);
      expect(add().hasAttribute('disabled')).toBe(false);
    }
    // Refused by the server, so they must not be offered.
    for (const invalid of ['-leading', '.leading', 'UPPER', 'has space', 'a'.repeat(65)]) {
      await userEvent.clear(key());
      await userEvent.type(key(), invalid);
      expect(add().hasAttribute('disabled')).toBe(true);
    }
  });

  it('blocks an add whose key is malformed or already taken, and says which', async () => {
    render(<TemplateEditorPage {...editorProps()} />);
    await userEvent.type(screen.getByLabelText('Goal'), 'Something.');

    await userEvent.type(screen.getByLabelText('Key'), 'Not A Key');
    expect(screen.getByRole('button', { name: 'Add task' }).hasAttribute('disabled')).toBe(true);
    expect(screen.getByText(/lowercase letters, digits and single hyphens/)).toBeTruthy();

    await userEvent.clear(screen.getByLabelText('Key'));
    await userEvent.type(screen.getByLabelText('Key'), 'inspect');
    expect(screen.getByRole('button', { name: 'Add task' }).hasAttribute('disabled')).toBe(true);
    expect(screen.getByText(/already used by another task/)).toBeTruthy();

    // Positive control: the same form with a valid, unused key is accepted.
    await userEvent.clear(screen.getByLabelText('Key'));
    await userEvent.type(screen.getByLabelText('Key'), 'run-tests');
    expect(screen.getByRole('button', { name: 'Add task' }).hasAttribute('disabled')).toBe(false);
  });

  it('renders no fields at all while the definition is loading', () => {
    render(<TemplateEditorPage {...editorProps({ template: undefined })} />);
    expect(screen.queryAllByRole('textbox').length).toBe(0);
    expect(screen.getByText('Loading template…')).toBeTruthy();
  });

  it('keeps what the user typed when the parent re-renders with an equal definition', async () => {
    const view = render(<TemplateEditorPage {...editorProps()} />);
    await userEvent.clear(screen.getByLabelText('Title'));
    await userEvent.type(screen.getByLabelText('Title'), 'Typed');
    // A fresh object with identical values — a query cache does this.
    view.rerender(<TemplateEditorPage {...editorProps({ template: { ...SMALL_CHANGE } })} />);
    expect(screen.getByLabelText<HTMLInputElement>('Title').value).toBe('Typed');
  });

  it('surfaces a save failure as an alert and keeps the draft', () => {
    render(<TemplateEditorPage {...editorProps({ saveError: 'PUT failed' })} />);
    expect(screen.getByRole('alert').textContent).toContain('PUT failed');
    expect(screen.getByLabelText<HTMLInputElement>('inspect').value).toBe('Read the requested change.');
  });

  /** Round 3: the post-save refetch used to wipe typing done in its window. */
  it('does not overwrite typing when the post-save refetch lands', async () => {
    const view = render(<TemplateEditorPage {...editorProps({ saving: true })} />);
    view.rerender(<TemplateEditorPage {...editorProps({ saving: false, savedAt: 1 })} />);
    await userEvent.clear(screen.getByLabelText('implement'));
    await userEvent.type(screen.getByLabelText('implement'), 'Typed after the save.');
    // The invalidation refetch resolves with the server's new definition.
    const server = { ...SMALL_CHANGE, tasks: [{ key: 'inspect', goal: 'Saved goal.' }, SMALL_CHANGE.tasks[1]] };
    view.rerender(<TemplateEditorPage {...editorProps({ saving: false, savedAt: 1, template: server })} />);
    expect(screen.getByLabelText<HTMLInputElement>('implement').value).toBe('Typed after the save.');
  });

  /** Round 3: pending appends were re-sent after they had been persisted. */
  it('clears pending appends once a save lands', async () => {
    const view = render(<TemplateEditorPage {...editorProps()} />);
    await userEvent.type(screen.getByLabelText('Key'), 'hand-off');
    await userEvent.type(screen.getByLabelText('Goal'), 'Summarize.');
    await userEvent.click(screen.getByRole('button', { name: 'Add task' }));
    expect(screen.getByLabelText('hand-off (new)')).toBeTruthy();

    view.rerender(<TemplateEditorPage {...editorProps({ savedAt: 7 })} />);
    // Still pending here would mean the next Save re-sends a persisted key and
    // the server 400s the whole request.
    expect(screen.queryByLabelText('hand-off (new)')).toBeNull();
  });

  /**
   * Round 3: the diff was index-paired.
   *
   * The misalignment needs the two lists to disagree on order, which happens
   * exactly when a re-seed is skipped: the user has typed (so their draft holds
   * the old order) and the list then refetches with a task prepended by another
   * client. Index pairing then compares `implement`'s new goal against
   * `inspect`'s old one and reports an edit to the wrong task.
   */
  it('pairs the diff by key, so a reordered refetch cannot misattribute a goal', async () => {
    const onSave = vi.fn();
    const view = render(<TemplateEditorPage {...editorProps({ onSave })} />);
    await userEvent.clear(screen.getByLabelText('implement'));
    await userEvent.type(screen.getByLabelText('implement'), 'Only this one.');

    // Another client prepended a task; the editor keeps the user's draft.
    const shifted = {
      ...SMALL_CHANGE,
      tasks: [{ key: 'added-first', goal: 'New.' }, ...SMALL_CHANGE.tasks],
    };
    view.rerender(<TemplateEditorPage {...editorProps({ onSave, template: shifted })} />);
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));

    expect((onSave.mock.calls[0][0] as TemplateSave).edits)
      .toEqual([{ key: 'implement', goal: 'Only this one.' }]);
  });

  it('keeps the save button focusable while saving', () => {
    render(<TemplateEditorPage {...editorProps({ saving: true })} />);
    const save = screen.getByRole('button', { name: /Saving…/ });
    expect(save.hasAttribute('disabled')).toBe(false);
    expect(save.getAttribute('aria-busy')).toBe('true');
  });

  /**
   * The other half of CR-6, and the half nothing was asserting.
   *
   * Save stays focusable and clickable during a save on purpose, and astryx's
   * `isInterruptible` disables its own in-flight dedupe — so the ONLY thing
   * stopping a second PUT with a stale `if_doc_rev` is the `if (saving) return`
   * in the click handler. Deleting that line used to leave every test green.
   */
  it('ignores a second click while a save is in flight', async () => {
    const onSave = vi.fn();
    render(<TemplateEditorPage {...editorProps({ saving: true, onSave })} />);
    await userEvent.click(screen.getByRole('button', { name: /Saving…/ }));
    expect(onSave).not.toHaveBeenCalled();
  });

  it('does not offer Save for a blank title or a blank goal', async () => {
    render(<TemplateEditorPage {...editorProps()} />);
    await userEvent.clear(screen.getByLabelText('Title'));
    // Dirty, but unsubmittable: the server refuses a blank title outright, so
    // an enabled Save here could only ever produce a 400.
    expect(screen.getByRole('button', { name: 'Save' }).hasAttribute('disabled')).toBe(true);
    expect(screen.getByText(/A template needs a title/)).toBeTruthy();

    await userEvent.type(screen.getByLabelText('Title'), 'Renamed');
    expect(screen.getByRole('button', { name: 'Save' }).hasAttribute('disabled')).toBe(false);

    await userEvent.clear(screen.getByLabelText('inspect'));
    expect(screen.getByRole('button', { name: 'Save' }).hasAttribute('disabled')).toBe(true);
    expect(screen.getByText('A task needs a goal.')).toBeTruthy();
  });

  /**
   * The wedge: a definition arriving mid-save must not be recorded as seen
   * without being applied, or no later re-seed can ever fire.
   */
  it('applies a definition that landed during a save once the save ends', () => {
    const grown = { ...SMALL_CHANGE, tasks: [...SMALL_CHANGE.tasks, { key: 'extra', goal: 'Third.' }] };
    const view = render(<TemplateEditorPage {...editorProps({ saving: true })} />);
    // Arrives while the save is in flight — must not clobber the draft now…
    view.rerender(<TemplateEditorPage {...editorProps({ saving: true, template: grown })} />);
    expect(screen.queryByLabelText('extra')).toBeNull();
    // …and must not be lost either.
    view.rerender(<TemplateEditorPage {...editorProps({ saving: false, template: grown })} />);
    expect(screen.getByLabelText<HTMLInputElement>('extra').value).toBe('Third.');
  });
});
