// @vitest-environment jsdom
import { act, cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { createStaticSource } from '@astryxdesign/core/Typeahead';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { DirectoryListing } from '../../../ui/directory-browser/public.tsx';
import type { TrackTemplate } from '../../../../../core/domain/track.ts';
import { ISSUE_INPUT_BODY, ISSUE_INPUT_SCHEMA } from './template-input-fixture.ts';
import { NewTrackForm } from './public.tsx';

afterEach(cleanup);

/** What the injected `listDirectory` port answers with. */
const LISTING: DirectoryListing = {
  path: '/srv/app',
  parent: '/srv',
  entries: [{ name: 'crates', path: '/srv/app/crates', isDirectory: true }],
};

/* The composer's accessible name, not rendered; astryx puts `label` on the
 * `contenteditable` as `aria-label`, so it resolves by label query. */
const TASK_LABEL = 'What this track should do';
const TASK_PLACEHOLDER = 'What should this track do?';

const FOLDER_PLACEHOLDER = 'Neige workspace';
const FOLDER_CHIP_NAME = `Folder: ${FOLDER_PLACEHOLDER}`;
const FOLDER_CLEAR_LABEL = 'Use a Neige workspace instead';

/* The template chip always names the current choice; the assertions vary the tail. */
const TEMPLATE_CHIP = /^Template: /;

/** The bound template, shaped as the read endpoint returns it. */
const ISSUE_DEV: TrackTemplate = {
  id: 'dev',
  title: 'Development',
  input_schema: JSON.parse(ISSUE_INPUT_SCHEMA) as unknown,
  tasks: [
    { key: 'inspect-issue', goal: 'Read the bound template input and view the source issue.' },
    { key: 'review-design-a', goal: 'Review the proposed design for correctness.' },
    { key: 'open-pr', goal: 'Open a pull request and check its diff.' },
    { key: 'merge', goal: 'Merge the pull request and close the issue.' },
  ],
};
/** Unbound templates: no `input_schema`, therefore no `template_input` on the wire. */
const SMALL_CHANGE: TrackTemplate = {
  id: 'small-change',
  title: 'Small change',
  tasks: [
    { key: 'inspect', goal: 'Read the requested change and the code it touches.' },
    { key: 'implement', goal: 'Implement the change and commit it.' },
    { key: 'verify', goal: "Run the repository's standard tests." },
  ],
};
const INVESTIGATION: TrackTemplate = {
  id: 'investigation',
  title: 'Investigation',
  tasks: [{ key: 'gather-facts', goal: 'Read the code, docs and history.' }],
};
/** A task-less template: offered without a task hover card. */
const INVESTMENT_RESEARCH: TrackTemplate = {
  id: 'investment-research',
  title: 'Investment research',
  tasks: [],
};
const TEMPLATES = [ISSUE_DEV, SMALL_CHANGE, INVESTIGATION, INVESTMENT_RESEARCH];
/** The templates that carry a task card; `INVESTMENT_RESEARCH` does not. */
const TEMPLATES_WITH_TASKS = TEMPLATES.filter((template) => template.tasks.length > 0);

function renderForm(overrides: Partial<Parameters<typeof NewTrackForm>[0]> = {}) {
  const onSubmit = vi.fn();
  const props = {
    submitting: false,
    error: null,
    templates: TEMPLATES,
    templatesLoaded: true,
    loadTemplate: vi.fn((id: string) => Promise.resolve({ id, title: 'Test template', description: 'Author supplied description.', instructions: 'Inspect the requested change.\nRun relevant verification.', body: id === 'dev' ? ISSUE_INPUT_BODY : '# Template source\n' })),
    initialTemplateId: null,
    initialCwd: null,
    /* Required, so a call site that forgot it cannot render a dead menu row. */
    onManageRecipes: vi.fn(),
    listDirectory: vi.fn(() => Promise.resolve(LISTING)),
    onSubmit,
    ...overrides,
  };
  return { props, onSubmit, ...render(<NewTrackForm {...props} />) };
}

/** The folder chip while nothing is chosen; once a folder is chosen the name carries the whole path. */
function folderChip(): HTMLButtonElement {
  return screen.getByRole('button', { name: FOLDER_CHIP_NAME });
}

async function pickTheListedFolder(): Promise<void> {
  await userEvent.click(folderChip());
  // `Select this directory` only enables once the path input and the listing agree.
  await screen.findByDisplayValue('/srv/app/');
  await userEvent.click(screen.getByRole('button', { name: 'Select this directory' }));
}

function submitButton(): HTMLButtonElement {
  return screen.getByRole('button', { name: /Create track|Creating/ });
}

/* `click` first: the field is a `contenteditable`, and `userEvent.type` needs a
 * caret inside it or the keystrokes land on `<body>`. */
async function fillMessage(value = 'Ship the thing') {
  const field = screen.getByLabelText(TASK_LABEL);
  await userEvent.click(field);
  await userEvent.type(field, value);
}

/** The collapsed Start from control, matched on the label prefix only. */
function templateTrigger(): HTMLButtonElement {
  return screen.getByRole('button', { name: TEMPLATE_CHIP });
}

/** `DropdownMenu` focuses its first item inside a `requestAnimationFrame`, so the menu is not open when the click resolves. */
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

describe('NewTrackForm asks only what the track starts from', () => {
  /* Create is gated on the sentence and on nothing else, in both directions. */
  it('keeps submit disabled while the composer is empty', () => {
    renderForm();
    expect(submitButton().disabled).toBe(true);
  });

  it('enables submit on the sentence alone', async () => {
    renderForm();
    await fillMessage();
    expect(submitButton().disabled).toBe(false);
  });

  /* The kernel enqueues and hashes the sentence untrimmed, so surrounding whitespace
   * is content; this form trims only to decide whether it may submit. */
  it('calls onSubmit with the sentence exactly as typed, whitespace and all', async () => {
    const { props } = renderForm();
    await fillMessage('  keep indentation  ');
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({ message: '  keep indentation  ' });
  });

  /* The kernel refuses `text.trim().is_empty()` with Rust's `char::is_whitespace`,
   * and JS `trim()` is a different set: `U+0085` is whitespace to Rust only. Both
   * gates are asserted: the button's `disabled` and the Enter path. */
  const BLANK_TO_THE_KERNEL: readonly (readonly [string, string])[] = [
    ['an ordinary space', ' '],
    ['a no-break space, U+00A0', '\u00A0'],
    ['a next line, U+0085 — whitespace to Rust, not to JS trim()', '\u0085'],
  ];
  it.each(BLANK_TO_THE_KERNEL)('refuses a draft of nothing but %s', async (_name, blank) => {
    const { props } = renderForm();
    await fillMessage(blank);
    expect(submitButton().disabled).toBe(true);
    await userEvent.type(screen.getByLabelText(TASK_LABEL), '{Enter}');
    expect(props.onSubmit).not.toHaveBeenCalled();
  });

  /* No combobox: the picker is a `DropdownMenu` (`role="menu"`), and astryx's
   * `Selector` renders `role="combobox"` whose listbox never gives an option DOM
   * focus, which would take the task hover cards off the keyboard. */
  it('asks for a template and an optional folder — never an area or claim control', async () => {
    const { props } = renderForm();
    await fillMessage();
    /* Unset, the chip names the default; its name and hover string say which
           control it is on top of that. */
    expect(folderChip().textContent).toBe(FOLDER_PLACEHOLDER);
    expect(folderChip().getAttribute('title')).toBe(FOLDER_CHIP_NAME);
    /* Matched on the mechanism's own words rather than /workspace/i, which the chip
           itself would now match. */
    expect(screen.queryByText(/allocates|git init|managed workspace/i)).toBeNull();
    // Empty means nothing was read: the picker only reaches its port on open.
    expect(props.listDirectory).not.toHaveBeenCalled();
    expect(screen.queryByLabelText('Area')).toBeNull();
    expect(screen.queryByLabelText(/Working directory/i)).toBeNull();
    expect(screen.queryByLabelText(/Claim this folder/)).toBeNull();
    expect(screen.queryByRole('combobox')).toBeNull();
    expect(screen.queryByRole('radio')).toBeNull();
    expect(screen.queryByRole('checkbox')).toBeNull();

    expect(templateTrigger().getAttribute('aria-expanded')).toBe('false');
    await openTemplates();
    expect(templateTrigger().getAttribute('aria-expanded')).toBe('true');
    /* The last row is the way to the recipe editor, present whether or not the
           reader has recipes. */
    expect(screen.getAllByRole('menuitem').map((item) => item.textContent))
      .toEqual([
        'No templateSelected', 'Development', 'Small change', 'Investigation',
        'Investment research', 'Manage recipes…',
      ]);
  });

  /* The field is a composer: `aria-multiline` is the assertion that would catch a
   * silent regression to a single-line control. */
  it('asks for the task in a multi-line composer, named but with no label row', () => {
    renderForm();
    const task = screen.getByLabelText(TASK_LABEL);
    expect(task.getAttribute('contenteditable')).toBe('true');
    expect(task.getAttribute('aria-multiline')).toBe('true');
    expect(screen.getByText(TASK_PLACEHOLDER)).toBeTruthy();
  });

  it('uses valid field ARIA when mention triggers are added and removed', () => {
    const trigger = { character: '@', searchSource: createStaticSource([]), onSelect: () => '' };
    const { props, rerender } = renderForm();
    expect(screen.getByRole('textbox', { name: TASK_LABEL }).getAttribute('aria-multiline')).toBe('true');
    rerender(<NewTrackForm {...props} mentionTrigger={trigger} />);
    expect(screen.getByRole('combobox', { name: TASK_LABEL }).hasAttribute('aria-multiline')).toBe(false);
    rerender(<NewTrackForm {...props} />);
    expect(screen.getByRole('textbox', { name: TASK_LABEL }).getAttribute('aria-multiline')).toBe('true');
  });

  it('flips the label and blocks submit while submitting', () => {
    renderForm({ submitting: true });
    expect(screen.getByRole('button', { name: 'Creating…' })).toHaveProperty('disabled', true);
  });

  it('surfaces the caller error in an alert region', () => {
    renderForm({ error: 'Could not create the track.' });
    expect(screen.getByRole('alert').textContent).toContain('Could not create');
  });

  it('offers the caller-owned explicit new-track action beside a payload conflict', async () => {
    const onRetryAsNewTrack = vi.fn();
    renderForm({
      error: 'This key belongs to different words.',
      errorAction: { label: 'Start as a new track', onClick: onRetryAsNewTrack },
    });
    await fillMessage();
    await userEvent.click(screen.getByRole('button', { name: 'Start as a new track' }));
    expect(onRetryAsNewTrack).toHaveBeenCalledWith({ message: 'Ship the thing' });
  });

  /* astryx's `ChatComposer.handleSubmit` clears the controlled value unconditionally
   * after calling us, so a refused submit would throw the sentence away. */
  it('keeps the draft when Enter is pressed on a submit the form refuses', async () => {
    const { onSubmit } = renderForm();
    await fillMessage('Ship the thing');
    await userEvent.click(templateTrigger());
    await userEvent.click(screen.getByRole('menuitem', { name: /^Development/ }));
    // A nonblank malformed optional issue still blocks submission.
    await userEvent.type(await screen.findByLabelText('Issue URL'), 'not a url');
    expect(submitButton().disabled).toBe(true);

    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.keyboard('{Enter}');

    expect(onSubmit).not.toHaveBeenCalled();
    expect(screen.getByLabelText(TASK_LABEL).textContent).toBe('Ship the thing');
  });

  /* The other half of owning Enter: disabling Enter outright would pass the case above. */
  it('creates on Enter when the draft is submittable', async () => {
    const { onSubmit } = renderForm();
    await fillMessage('Ship the thing');
    await userEvent.keyboard('{Enter}');
    expect(onSubmit).toHaveBeenCalledWith({ message: 'Ship the thing' });
  });

  /* Shift+Enter is a newline: the only way to reach the second line. */
  it('does not create on Shift+Enter', async () => {
    const { onSubmit } = renderForm();
    await fillMessage('Ship the thing');
    await userEvent.keyboard('{Shift>}{Enter}{/Shift}');
    expect(onSubmit).not.toHaveBeenCalled();
  });

  /* A capture handler on the composer wrapper would swallow Enter for the footer
   * chips and astryx's menu layer, which does not portal. */
  it('leaves Enter to the controls under the field', async () => {
    const { onSubmit } = renderForm();
    await fillMessage('Ship the thing');

    // The chip opens on Enter rather than creating a track.
    templateTrigger().focus();
    await userEvent.keyboard('{Enter}');
    expect(await screen.findByRole('menu')).toBeTruthy();
    expect(onSubmit).not.toHaveBeenCalled();

    // And choosing inside the menu selects, rather than creating.
    await userEvent.keyboard('{ArrowDown}');
    await userEvent.keyboard('{Enter}');
    expect(onSubmit).not.toHaveBeenCalled();
  });

  /* There are focusable controls inside the editable: astryx turns a long paste
   * into a token whose hover card carries an `Expand` button, a DOM descendant
   * of the `contenteditable`. Planted directly, to assert the rule for any descendant. */
  it('leaves Enter to a control inside the field', async () => {
    const { onSubmit } = renderForm();
    await fillMessage('Ship the thing');
    const field = screen.getByLabelText(TASK_LABEL);

    const expand = document.createElement('button');
    expand.textContent = 'Expand';
    /* `contenteditable="false"` is how astryx mounts its token nodes; without it the
           keydown targets the editable and the descendant is never exercised. */
    expand.setAttribute('contenteditable', 'false');
    field.appendChild(expand);
    let clicks = 0;
    expand.addEventListener('click', () => { clicks += 1; });
    expand.focus();
    expect(document.activeElement).toBe(expand);
    await userEvent.keyboard('{Enter}');

    expect(onSubmit).not.toHaveBeenCalled();
    /* And the control still works: `preventDefault` alongside `stopPropagation`
           would kill native button activation. */
    expect(clicks).toBe(1);
  });

  it('leaves Enter to the folder chip', async () => {
    const { onSubmit } = renderForm();
    await fillMessage('Ship the thing');
    folderChip().focus();
    await userEvent.keyboard('{Enter}');
    expect(await screen.findByRole('dialog')).toBeTruthy();
    expect(onSubmit).not.toHaveBeenCalled();
  });

  /* Asserted as an absence in both channels: the attribute could be left behind
   * pointing at an id nothing renders, a dangling IDREF. */
  it('promises no repetition, and leaves no description pointing at nothing', () => {
    renderForm();
    expect(screen.queryByText("You'll say this again in the track's chat")).toBeNull();
    const describedBy = screen.getByLabelText(TASK_LABEL).getAttribute('aria-describedby');
    expect(describedBy === null || document.getElementById(describedBy) !== null).toBe(true);
  });

  /* While a candidate is being composed, Enter accepts the candidate. Driven with
   * a real `compositionstart` and an `isComposing` keydown, which `userEvent.keyboard` cannot set. */
  it('does not create while an IME candidate is being composed', async () => {
    const { onSubmit } = renderForm();
    await fillMessage('ship');
    const field = screen.getByLabelText(TASK_LABEL);
    field.dispatchEvent(new CompositionEvent('compositionstart', { bubbles: true }));
    field.dispatchEvent(new KeyboardEvent('keydown', {
      key: 'Enter', bubbles: true, cancelable: true, isComposing: true,
    }));
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it('offers no Cancel — leaving a route is Back, not a button', () => {
    renderForm();
    expect(screen.queryByRole('button', { name: 'Cancel' })).toBeNull();
  });
});

describe('Start from — no template is the default and stays free', () => {
  it('selects no template on open and submits no template_id at all', async () => {
    const { onSubmit } = renderForm();
    /* The collapsed trigger is the answer to "what is selected"; a `<label htmlFor>`
           would replace the choice with the label. */
    expect(screen.getByRole('button', { name: 'Template: No template' })).toBe(templateTrigger());
    await fillMessage();
    await userEvent.click(submitButton());
    const [draft] = onSubmit.mock.calls[0] as [Record<string, unknown>];
    expect(draft).toEqual({ message: 'Ship the thing' });
    // Not `null`, not `''`: the kernel 400s a whitespace-only id and the body is
    // `deny_unknown_fields`. Absence is the only spelling of "no template".
    expect(Object.hasOwn(draft, 'template_id')).toBe(false);
  });

  /* An empty list is what a pending or failed template read looks like from here. */
  it('still creates a track when the template read gave nothing', async () => {
    const { props } = renderForm({ templates: [], templatesError: 'Could not load templates.' });
    expect(screen.getByRole('button', { name: 'Template: No template' })).toBe(templateTrigger());
    await openTemplates();
    /* Two: "No template", still the default, and "Manage recipes…", which creates nothing. */
    expect(screen.getAllByRole('menuitem').map((item) => item.textContent))
      .toEqual(['No templateSelected', 'Manage recipes…']);
    await userEvent.keyboard('{Escape}');
    await fillMessage();
    expect(submitButton().disabled).toBe(false);
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({ message: 'Ship the thing' });
  });

  it('says the templates are missing without claiming the create failed', () => {
    const { container } = renderForm({ templates: [], templatesError: 'Could not load templates.' });
    // Inspect this form, excluding global announcements retained by previous interactions.
    expect(within(container).queryByRole('alert')).toBeNull();
    expect(screen.getByText(/Could not load templates\..*still create a track without one/)).toBeTruthy();
  });
});

describe('Area creation defaults', () => {
  it('explains the Area template without blocking Enter-to-create', async () => {
    const { onSubmit } = renderForm({
      initialTemplateId: 'small-change',
      initialCwd: '/srv/area-default',
    });
    expect(screen.getByRole('button', { name: 'Template: Small change' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Folder: /srv/area-default' }).textContent)
      .toContain('area-default');
    await fillMessage();
    expect(submitButton().disabled).toBe(false);
    const notice = screen.getByRole('region', { name: 'Selected template' });
    expect(notice.textContent).not.toContain('Area default');
    expect(within(notice).getByRole('heading', { name: 'Small change' })).toBeTruthy();
    expect(notice.textContent).not.toContain('preset tasks');
    expect(within(notice).queryByRole('button', { name: 'Use Small change' })).toBeNull();
    await userEvent.keyboard('{Enter}');
    expect(onSubmit).toHaveBeenCalledWith({
      message: 'Ship the thing', template_id: 'small-change', cwd: '/srv/area-default',
    });
  });

  it('lets one Track reject the Area template through the picker', async () => {
    const { onSubmit } = renderForm({ initialTemplateId: 'small-change' });
    await fillMessage();
    await chooseTemplate('No template');
    expect(screen.getByRole('button', { name: 'Template: No template' })).toBeTruthy();
    await userEvent.click(submitButton());
    expect(onSubmit).toHaveBeenCalledWith({ message: 'Ship the thing' });
  });

  it('lets one Track explicitly return to No template and a new managed folder', async () => {
    const { onSubmit } = renderForm({
      initialTemplateId: 'small-change',
      initialCwd: '/srv/area-default',
    });
    await chooseTemplate('No template');
    await userEvent.click(screen.getByRole('button', { name: FOLDER_CLEAR_LABEL }));
    await fillMessage();
    await userEvent.click(submitButton());
    expect(onSubmit).toHaveBeenCalledWith({ message: 'Ship the thing' });
  });

  it('never silently clears an Area template whose roster has not loaded', async () => {
    const { onSubmit } = renderForm({
      templates: [],
      templatesLoaded: false,
      initialTemplateId: 'small-change',
    });
    await fillMessage();
    expect(submitButton().disabled).toBe(true);
    expect(screen.getByText('Loading the Area’s default template…')).toBeTruthy();

    await openTemplates();
    await userEvent.click(screen.getByRole('menuitem', { name: /^No template/ }));
    expect(submitButton().disabled).toBe(false);
    await userEvent.click(submitButton());
    expect(onSubmit).toHaveBeenCalledWith({ message: 'Ship the thing' });
  });

  it('keeps the Area template snapshot when the same route receives newer Area props', async () => {
    const rendered = renderForm({
      templates: [],
      templatesLoaded: false,
      initialTemplateId: 'opening-default',
    });
    await fillMessage();
    expect(submitButton().disabled).toBe(true);

    rendered.rerender(
      <NewTrackForm {...rendered.props} initialTemplateId={null} />,
    );

    expect(screen.getByRole('button', { name: 'Template: opening-default' })).toBeTruthy();
    expect(submitButton().disabled).toBe(true);
    expect(screen.getByText('Loading the Area’s default template…')).toBeTruthy();
  });

  it('blocks a saved Area template that is absent from a loaded roster', async () => {
    const { onSubmit } = renderForm({
      templates: [],
      templatesLoaded: true,
    loadTemplate: vi.fn((id: string) => Promise.resolve({ id, title: 'Test template', description: 'Author supplied description.', instructions: 'Inspect the requested change.\nRun relevant verification.', body: id === 'dev' ? ISSUE_INPUT_BODY : '# Template source\n' })),
      initialTemplateId: 'retired-template',
    });
    await fillMessage();
    expect(submitButton().disabled).toBe(true);
    expect(screen.getByRole('button', { name: 'Template: retired-template (unavailable)' }))
      .toBeTruthy();
    expect(screen.getByText(/Area’s default template is not available/)).toBeTruthy();

    await openTemplates();
    await userEvent.click(screen.getByRole('menuitem', { name: /^No template/ }));
    await userEvent.click(submitButton());
    expect(onSubmit).toHaveBeenCalledWith({ message: 'Ship the thing' });
  });

  it('keeps a saved Area template blocked when the roster read fails', async () => {
    const { onSubmit } = renderForm({
      templates: [],
      templatesLoaded: false,
      templatesError: 'Could not load templates.',
      initialTemplateId: 'small-change',
    });
    await fillMessage();
    expect(submitButton().disabled).toBe(true);
    expect(screen.getByText(/Could not load templates\..*Choose “No template”/)).toBeTruthy();

    await openTemplates();
    await userEvent.click(screen.getByRole('menuitem', { name: /^No template/ }));
    await userEvent.click(submitButton());
    expect(onSubmit).toHaveBeenCalledWith({ message: 'Ship the thing' });
  });

  it('preserves legal whitespace in the Area default folder path', async () => {
    const { onSubmit } = renderForm({ initialCwd: '/srv/ area ' });
    await fillMessage();
    await userEvent.click(submitButton());
    expect(onSubmit).toHaveBeenCalledWith({ message: 'Ship the thing', cwd: '/srv/ area ' });
  });
});

describe('Start from — an unbound template is id-only', () => {
  it('sends template_id and no template_input for small-change', async () => {
    const { onSubmit } = renderForm();
    await fillMessage();
    await chooseTemplate('Small change');
    // The collapsed trigger carries the choice out of the closed menu.
    expect(screen.getByRole('button', { name: 'Template: Small change' })).toBeTruthy();
    // No fields expand: the read said this template has no input schema.
    expect(screen.queryByLabelText('Issue URL')).toBeNull();
    expect(screen.queryByRole('checkbox')).toBeNull();
    await userEvent.click(submitButton());
    const [draft] = onSubmit.mock.calls[0] as [Record<string, unknown>];
    expect(draft).toEqual({ message: 'Ship the thing', template_id: 'small-change' });
    // Sending `template_input` against an unbound template is a 400.
    expect(Object.hasOwn(draft, 'template_input')).toBe(false);
  });
});

describe('Start from — issue development expands under the group', () => {
  async function chooseIssueDev(expectsFields = true) {
    await fillMessage();
    await chooseTemplate('Development');
    if (expectsFields) await screen.findByLabelText('Issue URL');
    else await screen.findByText('Author supplied description.');
  }

  it('allows an omitted issue but blocks a malformed nonblank URL', async () => {
    renderForm();
    await chooseIssueDev();
    expect(submitButton().disabled).toBe(false);
    expect(screen.getByText('Optional GitHub issue to associate with this change.')).toBeTruthy();
    // An untouched field is not yet wrong, and must not be announced as such.
    expect(screen.getByLabelText('Issue URL').getAttribute('aria-invalid')).toBeNull();

    await userEvent.type(screen.getByLabelText('Issue URL'), 'not a url');
    expect(submitButton().disabled).toBe(true);
    expect(within(screen.getByLabelText('Issue URL').closest('fieldset')!).getByText(/Not a GitHub issue URL/)).toBeTruthy();
    expect(screen.getByLabelText('Issue URL').getAttribute('aria-invalid')).toBe('true');
  });

  it('submits a development request without an issue and holds merge approval', async () => {
    const { onSubmit } = renderForm();
    await chooseIssueDev();
    await userEvent.click(submitButton());
    expect(onSubmit).toHaveBeenCalledWith({
      message: 'Ship the thing', template_id: 'dev',
      template_input: { merge_policy: 'hold-for-ratify' },
    });
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
      message: 'Ship the thing',
      template_id: 'dev',
      template_input: {
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
    const [draft] = onSubmit.mock.calls[0] as [{ template_input: { merge_policy: string } }];
    expect(draft.template_input.merge_policy).toBe('auto-merge');
  });

  /* Adjacency is not an association, so the panel is a `group` named after the
   * template; it carries no visible heading, the trigger already says it. */
  it('groups issue information and merge approval separately', async () => {
    renderForm();
    await chooseIssueDev();
    const panel = await screen.findByRole('group', { name: 'Source issue' });
    expect(within(panel).getByLabelText('Issue URL')).toBeTruthy();
    expect(within(panel).queryByRole('checkbox')).toBeNull();
    expect(within(screen.getByRole('group', { name: 'Merge approval' })).getByRole('checkbox')).toBeTruthy();
    // Directly under the control it belongs to, not after the whole picker.
    expect(templateTrigger().compareDocumentPosition(panel))
      .toBe(Node.DOCUMENT_POSITION_FOLLOWING);
  });

  /* A stopped plugin drops the schema on the read side, so the create path would
   * reject `template_input`; the picker follows, with no fields. */
  it('offers issue development with no fields when nothing is bound to it', async () => {
    const { props } = renderForm({
      templates: [{ id: 'dev', title: 'Development', tasks: ISSUE_DEV.tasks }],
    });
    await chooseIssueDev(false);
    expect(screen.queryByLabelText('Issue URL')).toBeNull();
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({
      message: 'Ship the thing', template_id: 'dev',
    });
  });

  /* Fail-closed on a template this build has no editor for: a readable block beats a 422. */
  it('refuses to submit a bound template it cannot collect input for', async () => {
    renderForm({
      templates: [{
        id: 'future-template',
        title: 'Future template',
        input_schema: { type: 'object' },
        tasks: [{ key: 'do-it', goal: 'Do the future thing.' }],
      }],
    });
    await fillMessage();
    await chooseTemplate('Future template');
    // The verdict needs the template body, which the preview reads asynchronously.
    expect(await screen.findByText(/needs input this version cannot collect/)).toBeTruthy();
    expect(submitButton().disabled).toBe(true);
  });
});

/* The content is the template's own pre-set tasks, not authored copy, so the
 * task text is what distinguishes "shows the plan" from "shows a blurb". */
describe('Start from — each template says which tasks it pre-sets', () => {
  it('hangs each template\'s own task list off that template\'s option', async () => {
    renderForm();
    const menu = await openTemplates();

    // Hover cards mount on demand in Astryx 0.6.3. Visit each option and
    // verify its own named popup, including the absence of another plan.
    for (const template of TEMPLATES_WITH_TASKS) {
      const option = within(menu).getByRole('menuitem', { name: new RegExp(`^${template.title}`) });
      act(() => { option.focus(); });
      const card = await screen.findByRole('dialog', { name: `${template.title} tasks` });
      expect(option.getAttribute('aria-controls')).toBe(card.id);
      for (const task of template.tasks) {
        expect(within(card).getByText(task.key)).toBeTruthy();
        expect(within(card).getByText(task.goal)).toBeTruthy();
      }
      for (const other of TEMPLATES) {
        if (other.id === template.id) continue;
        for (const task of other.tasks) expect(card.textContent).not.toContain(task.goal);
      }
    }
    expect(within(menu).getByRole('menuitem', { name: /^No template/ }).getAttribute('aria-describedby'))
      .toBeNull();
  });

  /* A task-less template is still offered and selectable, with no task hover card. */
  it('offers a task-less template as a selectable option with no task list', async () => {
    const { onSubmit } = renderForm();
    await fillMessage();
    const menu = await openTemplates();

    const option = within(menu).getByRole('menuitem', { name: /^Investment research/ });
    expect(option.getAttribute('aria-describedby')).toBeNull();
    expect(option.getAttribute('aria-haspopup')).toBeNull();
    for (const card of screen.queryAllByRole('dialog', { hidden: true })) {
      expect(card.textContent).not.toContain(INVESTMENT_RESEARCH.title);
    }
    await userEvent.click(option);
    expect(screen.getByRole('button', { name: 'Template: Investment research' })).toBeTruthy();
    await userEvent.click(submitButton());
    expect(onSubmit).toHaveBeenCalledWith({ message: 'Ship the thing', template_id: 'investment-research' });
  });

  /* Nothing inside the picker may be tabbable: it is entered from its trigger and
   * walked with arrow keys. The folder's clear companion is not in the walk
   * because it does not exist until a folder has been chosen. */
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
    // A disabled submit is not a tab stop, so fill the title first.
    await fillMessage();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    const order: Element[] = [];
    for (let step = 0; step < 3; step += 1) {
      await userEvent.tab();
      if (document.activeElement !== null) order.push(document.activeElement);
    }
    /* Three stops: the fourth tab leaves the composer entirely. */
    expect(order.slice(0, 3)).toEqual([
      templateTrigger(),
      folderChip(),
      submitButton(),
    ]);
  });

  /* `DropdownMenu` navigates with `useListFocus`, which calls `.focus()` on the
   * menuitem (unlike `Selector`, which only moves `aria-activedescendant`);
   * `focusTrigger="always"` is what makes `HoverCard` listen on a `tabindex="-1"` element. */
  it('opens the card by arrowing onto the option, with no pointer involved', async () => {
    renderForm();
    templateTrigger().focus();
    await userEvent.keyboard('{ArrowDown}');
    await act(async () => {
      await new Promise((resolve) => { requestAnimationFrame(() => resolve(null)); });
    });
    // First stop inside the menu is Blank, which has no card.
    expect(document.activeElement?.textContent).toContain('No template');
    expect(screen.queryByRole('dialog')).toBeNull();

    await userEvent.keyboard('{ArrowDown}{ArrowDown}{ArrowDown}');
    const option = document.activeElement as HTMLElement;
    expect(option.textContent).toContain('Investigation');
    const card = await screen.findByRole('dialog', { name: 'Investigation tasks' });
    expect(option.getAttribute('aria-controls')).toBe(card.id);
    expect(card.textContent).toContain('gather-facts');
  });
});

/* The kernel keys its managed-workspace branch on the absence of `cwd`; a path
 * plus `attach_folder` takes the attached branch. Create time is the only entry
 * into that second branch. */
describe('The folder is optional, and its absence is the managed default', () => {
  it('never requires a folder to submit', async () => {
    const { onSubmit } = renderForm();
    expect(submitButton().disabled).toBe(true);
    await fillMessage();
    // Nothing about the folder gates the submit, before or after the picker has opened.
    expect(submitButton().disabled).toBe(false);
    await userEvent.click(folderChip());
    await screen.findByDisplayValue('/srv/app/');
    await userEvent.keyboard('{Escape}');
    await userEvent.click(submitButton());
    const [draft] = onSubmit.mock.calls[0] as [Record<string, unknown>];
    expect(Object.hasOwn(draft, 'cwd')).toBe(false);
  });

  /* Asserted on key absence: `cwd: ''` is a path that cannot work, and `toMatchObject`
   * stays green on an extra key. */
  it('submits no cwd key at all when no folder was chosen', async () => {
    const { onSubmit } = renderForm();
    await fillMessage('  Ship the thing  ');
    await userEvent.click(submitButton());
    const [draft] = onSubmit.mock.calls[0] as [Record<string, unknown>];
    /* Padded on purpose: asserting the trimmed string would re-certify a trimming `message`. */
    expect(draft).toEqual({ message: '  Ship the thing  ' });
    expect(Object.hasOwn(draft, 'cwd')).toBe(false);
  });

  /* `DirectoryField` asks `useDialogView()` whether a dialog is above it and
   * otherwise renders `DirectoryBrowser` inline in the page, with no focus trap.
   * So the assertion is on the modal, not on which component opened it. */
  it('opens the picker in a modal dialog, never inline in the page', async () => {
    renderForm();
    expect(screen.queryByRole('dialog')).toBeNull();
    await userEvent.click(folderChip());
    const picker = await screen.findByRole('dialog');
    expect(picker.getAttribute('aria-modal')).toBe('true');
    expect(picker.getAttribute('aria-label')).toBe('Choose a directory');
    // The browser is inside it, not a sibling left in the page behind it.
    await within(picker).findByDisplayValue('/srv/app/');
    expect(within(picker).getByRole('button', { name: 'Select this directory' })).toBeTruthy();

    /* And the only one. `{ hidden: true }` is load-bearing: `Dialog` marks everything
           outside the portal `inert` + `aria-hidden`, and role queries skip that subtree
           by default, so a second inline browser would be invisible to the count. */
    expect(screen.getAllByRole('button', { name: 'Select this directory', hidden: true }))
      .toHaveLength(1);
  });

  it('submits the picked absolute path as cwd once a folder is chosen', async () => {
    const { onSubmit } = renderForm();
    await fillMessage();
    await pickTheListedFolder();
    expect(onSubmit).not.toHaveBeenCalled();
    await userEvent.click(submitButton());
    expect(onSubmit).toHaveBeenCalledWith({ message: 'Ship the thing', cwd: '/srv/app' });
  });

  /* Create time is the only entry into the attached choice, so the way back has to exist here too. */
  it('drops back to the managed default when the chosen folder is cleared', async () => {
    const { onSubmit } = renderForm();
    await fillMessage();
    await pickTheListedFolder();
    await userEvent.click(screen.getByRole('button', { name: 'Use a Neige workspace instead' }));
    await userEvent.click(submitButton());
    const [draft] = onSubmit.mock.calls[0] as [Record<string, unknown>];
    expect(draft).toEqual({ message: 'Ship the thing' });
    expect(Object.hasOwn(draft, 'cwd')).toBe(false);
  });

  it('offers no way back before a folder is chosen — there is nothing to clear', () => {
    renderForm();
    expect(screen.queryByRole('button', { name: 'Use a Neige workspace instead' })).toBeNull();
  });

  /* `ui/` primitives may not know a transport exists and `features/**` may not
   * import `app/**`, so the only route to the filesystem is the prop. */
  it('reads the directory through the injected port', async () => {
    const { props } = renderForm();
    await userEvent.click(folderChip());
    await screen.findByDisplayValue('/srv/app/');
    expect(props.listDirectory).toHaveBeenCalled();
  });

  it('carries the folder and the chosen template on one draft', async () => {
    const { onSubmit } = renderForm();
    await fillMessage();
    await chooseTemplate('Small change');
    await pickTheListedFolder();
    await userEvent.click(submitButton());
    expect(onSubmit).toHaveBeenCalledWith({
      message: 'Ship the thing', template_id: 'small-change', cwd: '/srv/app',
    });
  });
});
