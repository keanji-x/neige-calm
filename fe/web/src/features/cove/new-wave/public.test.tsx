// @vitest-environment jsdom
import { act, cleanup, render, screen, within } from '@testing-library/react';
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

/**
 * The collapsed Start from control.
 *
 * Matched on the label prefix, never on the whole string: the rest of the name
 * is the current choice, which is exactly what the assertions vary.
 */
function templateTrigger(): HTMLButtonElement {
  return screen.getByRole('button', { name: /^Start from/ });
}

/**
 * `DropdownMenu` focuses its first item inside a `requestAnimationFrame`, so
 * "the menu is open" is not true by the time the click resolves. Every case
 * that reads focus or the menu's contents goes through here.
 */
async function openTemplates() {
  await userEvent.click(templateTrigger());
  await act(async () => {
    await new Promise((resolve) => { requestAnimationFrame(() => resolve(null)); });
  });
  return screen.getByRole('menu');
}

/** Picks a template by name from the opened menu. */
async function chooseTemplate(name: string) {
  await openTemplates();
  await userEvent.click(screen.getByRole('menuitem', { name: new RegExp(`^${name}`) }));
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
   * checkbox or combobox at all — one assertion standing for "this dialog
   * stays minimal". #1209 added a template picker, so the shape has to be
   * restated rather than deleted, and this is the restatement:
   *
   *   * No **combobox**, still, and now for a second reason. The picker is a
   *     `DropdownMenu` (`role="menu"`), and the alternative astryx offers —
   *     `Selector` — renders `role="combobox"`. So this line is no longer just
   *     "the dialog is simple": it is the assertion that fails the day someone
   *     swaps the picker for a `Selector`, which would silently take the task
   *     hover cards off the keyboard (a `listbox` never gives an option DOM
   *     focus; see `public.tsx`). It is a contract with a reason, not a relic.
   *   * No **radio**: the alternatives are no longer spread across the dialog
   *     as permanent rows. They live behind one trigger.
   *   * No **checkbox** until a template that takes input is chosen.
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
    expect(screen.queryByRole('combobox')).toBeNull();
    expect(screen.queryByRole('radio')).toBeNull();
    expect(screen.queryByRole('checkbox')).toBeNull();

    // The picker is collapsed: one control, named by its field label *and* by
    // what it currently holds.
    expect(templateTrigger().getAttribute('aria-expanded')).toBe('false');
    await openTemplates();
    expect(templateTrigger().getAttribute('aria-expanded')).toBe('true');
    expect(screen.getAllByRole('menuitem').map((item) => item.textContent))
      .toEqual(['BlankSelected', 'Issue development', 'Small change', 'Investigation']);
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
    /* The collapsed trigger *is* the answer to "what is selected" — there is
       no checked radio left to read it off. Its accessible name is the field
       label plus the current choice, which is also why a `<label htmlFor>`
       cannot be used here: it would replace the choice with the label. */
    expect(screen.getByRole('button', { name: 'Start from Blank' })).toBe(templateTrigger());
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
    expect(screen.getByRole('button', { name: 'Start from Blank' })).toBe(templateTrigger());
    await openTemplates();
    expect(screen.getAllByRole('menuitem')).toHaveLength(1);
    await userEvent.keyboard('{Escape}');
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
    await chooseTemplate('Small change');
    // The collapsed trigger carries the choice out of the closed menu.
    expect(screen.getByRole('button', { name: 'Start from Small change' })).toBeTruthy();
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
    await chooseTemplate('Issue development');
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
   * The expanded fields belong to one alternative, and a reader who lands on
   * them has to be told which. With the picker collapsed the fields now sit
   * directly beneath the trigger that names the template — but adjacency is
   * not an association, so the panel is still a `group` carrying that title as
   * its accessible name. It carries no *visible* heading any more: the trigger
   * one row above already says it.
   */
  it('names the expanded panel after the template that opened it', async () => {
    renderForm();
    await chooseIssueDev();
    const panel = screen.getByRole('group', { name: 'Issue development' });
    expect(within(panel).getByLabelText('Issue URL')).toBeTruthy();
    expect(within(panel).getByRole('checkbox')).toBeTruthy();
    // Directly under the control it belongs to, not after the whole picker.
    expect(templateTrigger().compareDocumentPosition(panel))
      .toBe(Node.DOCUMENT_POSITION_FOLLOWING);
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
    await chooseTemplate('Future template');
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
  it('hangs each template\'s own task list off that template\'s option', async () => {
    renderForm();
    const menu = await openTemplates();

    // Every task key of the bound template, and its goal, is available.
    for (const task of ISSUE_DEV.tasks) {
      expect(screen.getByText(task.key)).toBeTruthy();
      expect(screen.getByText(task.goal)).toBeTruthy();
    }

    /*
     * One card per template, and each card is bound to *its own* option — the
     * check that separates "the card lists the right tasks" from "every row
     * points at the same list". Blank has no card: it has no tasks.
     */
    const cards = screen.getAllByRole('dialog', { hidden: true });
    expect(cards).toHaveLength(TEMPLATES.length);
    for (const template of TEMPLATES) {
      const option = within(menu).getByRole('menuitem', { name: new RegExp(`^${template.title}`) });
      const card = document.getElementById(option.getAttribute('aria-describedby') ?? '');
      expect(card).toBeTruthy();
      for (const task of template.tasks) expect(card?.textContent).toContain(task.key);
      // Goals and not keys for the negative: `inspect` is a prefix of
      // `inspect-issue`, so a key-based "not to contain" would fail on a card
      // that is in fact correct. Goals are whole sentences and unique.
      for (const other of TEMPLATES) {
        if (other.id === template.id) continue;
        for (const task of other.tasks) expect(card?.textContent).not.toContain(task.goal);
      }
    }
    expect(within(menu).getByRole('menuitem', { name: /^Blank/ }).getAttribute('aria-describedby'))
      .toBeNull();
  });

  /*
   * ── The tab stop this test used to *demand* ────────────────────────────
   *
   * The previous revision hung the card off a separate "N tasks" label in each
   * row's `endContent`, and `HoverCard` renders a string child as a focusable
   * `<span tabIndex={0}>`. That put one extra tab stop *inside every row* of a
   * composite widget that is supposed to be a single stop — and the test here
   * asserted that stop existed, which turned the defect into a guarded
   * feature. It is inverted deliberately: nothing inside the picker may be
   * tabbable, because the picker is entered from its trigger and walked with
   * arrow keys.
   *
   * Green when: the dialog's tab order is task → picker → actions, and every
   * option carries `tabindex="-1"`.
   * Red when: the "N tasks" trigger (or any other `tabindex="0"`) comes back
   * inside the menu, or an option is left tabbable.
   */
  it('costs the tab order nothing — the picker is one stop, options are not', async () => {
    renderForm();
    const menu = await openTemplates();

    for (const option of within(menu).getAllByRole('menuitem')) {
      expect(option.getAttribute('tabindex')).toBe('-1');
    }
    expect(menu.querySelectorAll('[tabindex="0"]')).toHaveLength(0);
    // No "N tasks" affordance anywhere: the option itself is the trigger.
    expect(screen.queryByText(/\d+ tasks?$/)).toBeNull();

    await userEvent.keyboard('{Escape}');
    // A disabled submit is not a tab stop, so the walk below would end on the
    // document — fill the title first and the actions row is reachable.
    await fillTitle();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    const order: Element[] = [];
    for (let step = 0; step < 3; step += 1) {
      await userEvent.tab();
      if (document.activeElement !== null) order.push(document.activeElement);
    }
    expect(order).toEqual([
      templateTrigger(),
      screen.getByRole('button', { name: 'Cancel' }),
      submitButton(),
    ]);
  });

  /*
   * A hover-only affordance does not exist for a keyboard user. The card now
   * hangs off the option itself, and the option is where DOM focus actually
   * goes: `DropdownMenu` navigates with `useListFocus`, which calls
   * `.focus()` on the `[role="menuitem"]` element (unlike `Selector`, whose
   * listbox only moves `aria-activedescendant` and would leave this card
   * unreachable). `focusTrigger="always"` is what makes `HoverCard` listen —
   * its `'auto'` default declines any element with `tabindex="-1"`.
   *
   * Red when: the picker is swapped for an activedescendant control, or
   * `focusTrigger` drops back to the default.
   */
  it('opens the card by arrowing onto the option, with no pointer involved', async () => {
    renderForm();
    templateTrigger().focus();
    await userEvent.keyboard('{ArrowDown}');
    await act(async () => {
      await new Promise((resolve) => { requestAnimationFrame(() => resolve(null)); });
    });
    // First stop inside the menu is Blank, which has no card.
    expect(document.activeElement?.textContent).toContain('Blank');
    expect(screen.queryByRole('dialog')).toBeNull();

    await userEvent.keyboard('{ArrowDown}{ArrowDown}{ArrowDown}');
    const option = document.activeElement as HTMLElement;
    expect(option.textContent).toContain('Investigation');
    // Shown, not merely present: every card is in the DOM at all times inside
    // a closed `popover`, so an assertion that did not filter by accessibility
    // state would pass without the focus listener ever firing.
    const describedBy = option.getAttribute('aria-describedby') ?? '';
    /* Shown, not merely present: every card is in the DOM at all times inside
       a closed `popover`, so `getElementById` alone would pass without the
       focus listener ever firing. A role query only returns what is in the
       accessibility tree, and `getAllBy` because the card of the option
       arrowed *past* is still fading out on its 200 ms hide delay. */
    const shown = screen.getAllByRole('dialog');
    expect(shown.map((card) => card.id)).toContain(describedBy);
    expect(document.getElementById(describedBy)?.textContent).toContain('gather-facts');
  });
});
