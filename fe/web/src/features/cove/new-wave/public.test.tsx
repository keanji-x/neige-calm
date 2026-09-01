// @vitest-environment jsdom
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { WaveTemplate } from '../../../../../core/domain/wave.ts';
import { NewWaveForm } from './public.tsx';

afterEach(cleanup);

/** The bound template, shaped as the read endpoint returns it. */
const ISSUE_DEV: WaveTemplate = {
  id: 'issue-development',
  title: 'Issue development',
  input_schema: {
    type: 'object',
    properties: { issue_url: { type: 'string' } },
    required: ['issue_url', 'repo', 'issue_number'],
  },
};
/** Unbound templates: no `input_schema`, therefore no fields, therefore no
    `workflow_input` on the wire. */
const SMALL_CHANGE: WaveTemplate = { id: 'small-change', title: 'Small change' };
const INVESTIGATION: WaveTemplate = { id: 'investigation', title: 'Investigation' };
const TEMPLATES = [ISSUE_DEV, SMALL_CHANGE, INVESTIGATION];

function renderForm(overrides: Partial<Parameters<typeof NewWaveForm>[0]> = {}) {
  const onSubmit = vi.fn();
  const props = {
    submitting: false,
    error: null,
    templates: TEMPLATES,
    titleRef: { current: null },
    onCancel: vi.fn(),
    onSubmit,
    ...overrides,
  };
  return { props, onSubmit, ...render(<NewWaveForm {...props} />) };
}

function submitButton(): HTMLButtonElement {
  return screen.getByRole('button', { name: /Create wave|Creating/ });
}

async function fillTitle(value = 'Ship the thing') {
  await userEvent.type(screen.getByLabelText('Task'), value);
}

describe('NewWaveForm asks for a task and what the wave starts from', () => {
  it('keeps submit disabled while the task is empty', () => {
    renderForm();
    expect(submitButton().disabled).toBe(true);
  });

  it('enables submit after typing a title', async () => {
    renderForm();
    await fillTitle();
    expect(submitButton().disabled).toBe(false);
  });

  it('calls onSubmit with the trimmed title', async () => {
    const { props } = renderForm();
    await fillTitle('  Ship the thing  ');
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({ title: 'Ship the thing' });
  });

  /*
   * The pre-#1209 shape of this test asserted the dialog had *no* radio,
   * checkbox or combobox at all. Two of those three are still true and stay
   * asserted; the third was the whole point of #1209, so it flips from "must
   * not exist" to "must exist, and must be a real radio group" — the control
   * this form was most likely to fake with divs and onClick.
   *
   * What has not changed: this dialog still does not ask for a folder, a cove,
   * or a working directory. Those are the assertions that were load-bearing.
   */
  it('asks for the task and a template — never a folder, cove, or claim control', async () => {
    renderForm();
    await fillTitle();
    expect(screen.queryByLabelText('Folder')).toBeNull();
    expect(screen.queryByLabelText('Cove')).toBeNull();
    expect(screen.queryByLabelText(/Working directory/i)).toBeNull();
    expect(screen.queryByLabelText(/Claim this folder/)).toBeNull();
    // No OS dropdown anywhere in the dialog: the picker is a radio list.
    expect(screen.queryByRole('combobox')).toBeNull();
    expect(screen.getByRole('radiogroup', { name: 'Start from' })).toBeTruthy();
    expect(screen.getAllByRole('radio').map((radio) => radio.getAttribute('value')))
      .toEqual(['', 'issue-development', 'small-change', 'investigation']);
    // Nothing is checkable until a template that takes input is selected.
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

describe('Start from — Blank is the default and stays free', () => {
  it('selects Blank on open and submits no workflow_id at all', async () => {
    const { onSubmit } = renderForm();
    expect(screen.getByRole('radio', { name: 'Blank' })).toHaveProperty('checked', true);
    await fillTitle();
    await userEvent.click(submitButton());
    const [draft] = onSubmit.mock.calls[0] as [Record<string, unknown>];
    expect(draft).toEqual({ title: 'Ship the thing' });
    // Not `null`, not `''` — the kernel 400s a whitespace-only id and the body
    // is `deny_unknown_fields`. Absence is the only spelling of "no template".
    expect(Object.hasOwn(draft, 'workflow_id')).toBe(false);
  });

  /*
   * The failure mode this guards is the real one: `GET /api/wave-templates` is
   * down or slow, and the app's only wave-creation entry point becomes a
   * dialog that cannot create a wave. An empty list is what a pending or
   * failed read looks like from here.
   */
  it('still creates a blank wave when the template read gave nothing', async () => {
    const { props } = renderForm({ templates: [], templatesError: 'Could not load templates.' });
    expect(screen.getByRole('radio', { name: 'Blank' })).toHaveProperty('checked', true);
    expect(screen.getAllByRole('radio')).toHaveLength(1);
    await fillTitle();
    expect(submitButton().disabled).toBe(false);
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({ title: 'Ship the thing' });
  });

  it('says the templates are missing without claiming the create failed', () => {
    renderForm({ templates: [], templatesError: 'Could not load templates.' });
    // A `status`, not an `alert`: nothing the user did failed.
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.getByText(/Could not load templates\..*still create a blank wave/)).toBeTruthy();
  });
});

describe('Start from — an unbound template is id-only', () => {
  it('sends workflow_id and no workflow_input for small-change', async () => {
    const { onSubmit } = renderForm();
    await fillTitle();
    await userEvent.click(screen.getByRole('radio', { name: 'Small change' }));
    // No fields expand: the read said this template has no input schema.
    expect(screen.queryByLabelText('Issue URL')).toBeNull();
    expect(screen.queryByRole('checkbox')).toBeNull();
    await userEvent.click(submitButton());
    const [draft] = onSubmit.mock.calls[0] as [Record<string, unknown>];
    expect(draft).toEqual({ title: 'Ship the thing', workflow_id: 'small-change' });
    // Sending `workflow_input` against an unbound template is a 400.
    expect(Object.hasOwn(draft, 'workflow_input')).toBe(false);
  });
});

describe('Start from — issue development expands in place', () => {
  async function chooseIssueDev() {
    await fillTitle();
    await userEvent.click(screen.getByRole('radio', { name: 'Issue development' }));
  }

  it('blocks submit until the issue URL parses, and says why', async () => {
    renderForm();
    await chooseIssueDev();
    expect(submitButton().disabled).toBe(true);
    expect(screen.getByText(/Paste the GitHub issue/)).toBeTruthy();

    await userEvent.type(screen.getByLabelText('Issue URL'), 'not a url');
    expect(submitButton().disabled).toBe(true);
    expect(screen.getByText(/Not a GitHub issue URL/)).toBeTruthy();
    expect(screen.getByLabelText('Issue URL').getAttribute('aria-invalid')).toBe('true');
  });

  it('derives repo and issue_number client-side and holds for ratify by default', async () => {
    const { props } = renderForm();
    await chooseIssueDev();
    await userEvent.type(
      screen.getByLabelText('Issue URL'),
      'https://github.com/keanji-x/neige-calm/issues/1209',
    );
    expect(submitButton().disabled).toBe(false);
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({
      title: 'Ship the thing',
      workflow_id: 'issue-development',
      workflow_input: {
        issue_url: 'https://github.com/keanji-x/neige-calm/issues/1209',
        repo: 'keanji-x/neige-calm',
        issue_number: 1209,
        // The direction that matters: unchecked means a human approves.
        merge_policy: 'hold-for-ratify',
      },
    });
  });

  it('sends auto-merge only when the box is checked', async () => {
    const { onSubmit } = renderForm();
    await chooseIssueDev();
    await userEvent.type(screen.getByLabelText('Issue URL'), 'https://github.com/o/r/issues/7');
    await userEvent.click(screen.getByRole('checkbox'));
    await userEvent.click(submitButton());
    const [draft] = onSubmit.mock.calls[0] as [{ workflow_input: { merge_policy: string } }];
    expect(draft.workflow_input.merge_policy).toBe('auto-merge');
  });

  /*
   * The expanded fields belong to one row of the group, and a reader who lands
   * on the panel has to be told which. `aria-controls` on the radio plus a
   * `group` labelled by that radio is the association; without it the panel is
   * a set of orphan fields sitting between two alternatives.
   */
  it('ties the expanded panel to the radio that opened it', async () => {
    renderForm();
    await chooseIssueDev();
    const radio = screen.getByRole('radio', { name: 'Issue development' });
    const panelId = radio.getAttribute('aria-controls');
    expect(panelId).toBeTruthy();
    const panel = screen.getByRole('group');
    expect(panel.id).toBe(panelId);
    expect(panel.getAttribute('aria-labelledby')).toBe(radio.closest('label')?.id);
  });

  /*
   * A stopped plugin drops the schema on the read side, so the create path
   * would reject `workflow_input`. The picker must follow: still offerable
   * (the report still seeds), just with no fields.
   */
  it('offers issue development with no fields when nothing is bound to it', async () => {
    const { props } = renderForm({
      templates: [{ id: 'issue-development', title: 'Issue development' }],
    });
    await chooseIssueDev();
    expect(screen.queryByLabelText('Issue URL')).toBeNull();
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({
      title: 'Ship the thing', workflow_id: 'issue-development',
    });
  });

  /*
   * Fail-closed on a template this build has no editor for: the kernel would
   * demand the input its schema declares, so a readable block beats a 422.
   */
  it('refuses to submit a bound template it cannot collect input for', async () => {
    renderForm({
      templates: [{ id: 'future-template', title: 'Future template', input_schema: { type: 'object' } }],
    });
    await fillTitle();
    await userEvent.click(screen.getByRole('radio', { name: 'Future template' }));
    expect(submitButton().disabled).toBe(true);
    expect(screen.getByText(/needs input this version cannot collect/)).toBeTruthy();
  });
});
