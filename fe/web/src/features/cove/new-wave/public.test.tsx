// @vitest-environment jsdom
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { WaveTemplate } from '../../../../../core/domain/wave.ts';
import { NewWaveForm } from './public.tsx';

afterEach(cleanup);

/*
 * The Task field's accessible name. It is visually hidden — the field is one
 * line and the placeholder already says what it wants — so the name is
 * spelled out here deliberately: an unnamed textbox is unusable by screen
 * reader and by voice control, and this is the assertion that would catch its
 * removal.
 */
const TASK_LABEL = 'What this wave should do';

/** The bound template, shaped as the read endpoint returns it. */
const ISSUE_DEV: WaveTemplate = {
  id: 'issue-development',
  title: 'Issue development',
  input_schema: {
    type: 'object',
    properties: { issue_url: { type: 'string' } },
    required: ['issue_url', 'repo', 'issue_number'],
  },
  tasks: [
    { key: 'inspect-issue', goal: 'Read the bound workflow input and view the source issue.' },
    { key: 'review-design-a', goal: 'Review the proposed design for correctness.' },
    { key: 'open-pr', goal: 'Open a pull request and check its diff.' },
    { key: 'merge', goal: 'Merge the pull request and close the issue.' },
  ],
};
/** Unbound templates: no `input_schema`, therefore no fields, therefore no
    `workflow_input` on the wire. */
const SMALL_CHANGE: WaveTemplate = {
  id: 'small-change',
  title: 'Small change',
  tasks: [
    { key: 'inspect', goal: 'Read the requested change and the code it touches.' },
    { key: 'implement', goal: 'Implement the change and commit it.' },
    { key: 'verify', goal: "Run the repository's standard tests." },
  ],
};
const INVESTIGATION: WaveTemplate = {
  id: 'investigation',
  title: 'Investigation',
  tasks: [{ key: 'gather-facts', goal: 'Read the code, docs and history.' }],
};
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
  await userEvent.type(screen.getByLabelText(TASK_LABEL), value);
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
   * Single line, not a textarea, and named without a visible label row. The
   * value becomes the wave's `title`, and every other surface renders it as
   * one truncated line — sidebar, wave list, page header — while the wave page
   * edits it through the single-line `EditableTitle`.
   * `getByRole('textbox')` is true of both elements, so this asserts the tag
   * itself; anything weaker would stay green on a textarea.
   */
  it('asks for the task on one line, named but with no label row', () => {
    renderForm();
    const task = screen.getByLabelText<HTMLInputElement>(TASK_LABEL);
    expect(task).toHaveProperty('tagName', 'INPUT');
    expect(task).toHaveProperty('type', 'text');
    // The row the user asked us to reclaim: the prompt lives in the box.
    expect(task.placeholder).toBe('What should this wave do?');
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

describe('Start from — issue development expands under the group', () => {
  async function chooseIssueDev() {
    await fillTitle();
    await userEvent.click(screen.getByRole('radio', { name: 'Issue development' }));
  }

  it('blocks submit until the issue URL parses, and says why', async () => {
    renderForm();
    await chooseIssueDev();
    expect(submitButton().disabled).toBe(true);
    expect(screen.getByText(/Paste the GitHub issue/)).toBeTruthy();
    // An untouched field is not yet wrong, and must not be announced as such.
    expect(screen.getByLabelText('Issue URL').getAttribute('aria-invalid')).toBeNull();

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
   * The expanded fields belong to one alternative in the group, and a reader
   * who lands on them has to be told which. astryx's `RadioListItem` takes no
   * children and exposes no hook for `aria-controls` on its radio, so the
   * association is carried the other way: the panel is a `group` whose
   * accessible name is the chosen template's title. Without it the fields are
   * orphans sitting under a list of alternatives.
   */
  it('names the expanded panel after the template that opened it', async () => {
    renderForm();
    await chooseIssueDev();
    const panel = screen.getByRole('group', { name: 'Issue development' });
    expect(within(panel).getByLabelText('Issue URL')).toBeTruthy();
    expect(within(panel).getByRole('checkbox')).toBeTruthy();
  });

  /*
   * A stopped plugin drops the schema on the read side, so the create path
   * would reject `workflow_input`. The picker must follow: still offerable
   * (the report still seeds), just with no fields.
   */
  it('offers issue development with no fields when nothing is bound to it', async () => {
    const { props } = renderForm({
      templates: [{ id: 'issue-development', title: 'Issue development', tasks: ISSUE_DEV.tasks }],
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
      templates: [{
        id: 'future-template',
        title: 'Future template',
        input_schema: { type: 'object' },
        tasks: [{ key: 'do-it', goal: 'Do the future thing.' }],
      }],
    });
    await fillTitle();
    await userEvent.click(screen.getByRole('radio', { name: 'Future template' }));
    expect(submitButton().disabled).toBe(true);
    expect(screen.getByText(/needs input this version cannot collect/)).toBeTruthy();
  });
});

/*
 * #1209 — "what does this template actually give me", answered on the row.
 *
 * The content is the template's own pre-set tasks, not authored copy: #1209
 * ruled out adding a `description` to the kernel's template table, and a
 * second prose source in the client would have been the same mistake one layer
 * up. These tests therefore assert the *task* text, per template, which is the
 * only thing that distinguishes "shows the plan" from "shows a blurb".
 */
describe('Start from — each template says which tasks it pre-sets', () => {
  it('names the count on the row and lists that template\'s tasks behind it', () => {
    renderForm();
    // Distinct counts per template, so a trigger that showed the *list's*
    // length instead of its own row's would be caught here.
    expect(screen.getByText('4 tasks')).toBeTruthy();
    expect(screen.getByText('3 tasks')).toBeTruthy();
    expect(screen.getByText('1 task')).toBeTruthy();

    // Every task key of the bound template, and its goal, is available.
    for (const task of ISSUE_DEV.tasks) {
      expect(screen.getByText(task.key)).toBeTruthy();
      expect(screen.getByText(task.goal)).toBeTruthy();
    }
    // And each list belongs to its own template — investigation's single task
    // is not in issue-development's card.
    const cards = screen.getAllByRole('dialog', { hidden: true });
    expect(cards).toHaveLength(TEMPLATES.length);
    const issueDevCard = cards.find((card) => card.textContent?.includes('inspect-issue'));
    expect(issueDevCard?.textContent).not.toContain('gather-facts');
  });

  /*
   * A hover-only affordance does not exist for a keyboard or a touch user. The
   * trigger has to be a tab stop, and the card it opens has to be what
   * assistive technology is pointed at when that stop is reached.
   */
  it('puts the task list on a tab stop, not on hover alone', async () => {
    renderForm();
    const trigger = screen.getByText('1 task');
    expect(trigger.getAttribute('tabindex')).toBe('0');

    const describedBy = trigger.getAttribute('aria-describedby');
    expect(describedBy).toBeTruthy();
    const card = document.getElementById(describedBy ?? '');
    expect(card?.textContent).toContain('gather-facts');

    // Reachable in practice, not just in attributes: tabbing from the field
    // walks into the group and onto the trigger.
    await userEvent.tab();
    for (let step = 0; step < 8 && document.activeElement !== trigger; step += 1) {
      await userEvent.tab();
    }
    expect(document.activeElement).toBe(trigger);
  });
});
