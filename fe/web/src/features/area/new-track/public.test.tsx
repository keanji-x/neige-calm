// @vitest-environment jsdom
import { act, cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { DirectoryListing } from '../../../ui/directory-browser/public.tsx';
import type { TrackTemplate } from '../../../../../core/domain/track.ts';
import { NewTrackForm } from './public.tsx';

afterEach(cleanup);

/** What the injected `listDirectory` port answers with. */
const LISTING: DirectoryListing = {
  path: '/srv/app',
  parent: '/srv',
  entries: [{ name: 'crates', path: '/srv/app/crates', isDirectory: true }],
};

/*
 * The composer's accessible name. It is not rendered — the placeholder already
 * says what the field wants — so the name is spelled out here deliberately: an
 * unnamed textbox is unusable by screen reader and by voice control, and this
 * is the assertion that would catch its removal. astryx puts `label` on the
 * `contenteditable` as `aria-label`, so it resolves by label query.
 */
const TASK_LABEL = 'What this track should do';
const TASK_PLACEHOLDER = 'What should this track do?';

/* The folder chip's copy, restated here for the same reason `TASK_LABEL` is:
   it is user-facing text, and a test that imported it from the component could
   not fail when the component silently changed it. Since #1211 the chip names
   the **default** rather than asking, and its accessible name says which
   control it is on top of that. */
const FOLDER_PLACEHOLDER = 'Neige workspace';
const FOLDER_CHIP_NAME = `Folder: ${FOLDER_PLACEHOLDER}`;

/* The template chip. It always names the current choice — "No template" until
   one is picked — so the name has one shape, and the assertions vary the tail
   after the colon. */
const TEMPLATE_CHIP = /^Template: /;

/** The bound template, shaped as the read endpoint returns it. */
const ISSUE_DEV: TrackTemplate = {
  id: 'issue-development',
  title: 'Issue development',
  input_schema: {
    type: 'object',
    properties: { issue_url: { type: 'string' } },
    required: ['issue_url', 'repo', 'issue_number'],
  },
  tasks: [
    { key: 'inspect-issue', goal: 'Read the bound template input and view the source issue.' },
    { key: 'review-design-a', goal: 'Review the proposed design for correctness.' },
    { key: 'open-pr', goal: 'Open a pull request and check its diff.' },
    { key: 'merge', goal: 'Merge the pull request and close the issue.' },
  ],
};
/** Unbound templates: no `input_schema`, therefore no fields, therefore no
    `template_input` on the wire. */
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
const TEMPLATES = [ISSUE_DEV, SMALL_CHANGE, INVESTIGATION];

function renderForm(overrides: Partial<Parameters<typeof NewTrackForm>[0]> = {}) {
  const onSubmit = vi.fn();
  const props = {
    submitting: false,
    error: null,
    templates: TEMPLATES,
    /* Required, like `listDirectory` and for the same reason: a call site that
       forgot it would render the one route into the recipe editor as a dead
       menu row. */
    onManageRecipes: vi.fn(),
    listDirectory: vi.fn(() => Promise.resolve(LISTING)),
    onSubmit,
    ...overrides,
  };
  return { props, onSubmit, ...render(<NewTrackForm {...props} />) };
}

/**
 * The folder chip while nothing is chosen.
 *
 * Named, not labelled: the chip carries its own `aria-label` rather than
 * sitting in a labelled field. Unset, that name is the control's word plus the
 * default it is holding; once a folder is chosen the visible text becomes the
 * basename and the name carries the whole path, which is why this helper is
 * only good for the unset state.
 */
function folderChip(): HTMLButtonElement {
  return screen.getByRole('button', { name: FOLDER_CHIP_NAME });
}

async function pickTheListedFolder(): Promise<void> {
  await userEvent.click(folderChip());
  // The browser loads on mount and mirrors the listing into its path input;
  // `Select this directory` only enables once the two agree.
  await screen.findByDisplayValue('/srv/app/');
  await userEvent.click(screen.getByRole('button', { name: 'Select this directory' }));
}

function submitButton(): HTMLButtonElement {
  return screen.getByRole('button', { name: /Create track|Creating/ });
}

/*
 * Types into the composer.
 *
 * `click` first, then `type`: the field is a `contenteditable`, and
 * `userEvent.type` needs a caret inside it — without the click the keystrokes
 * land on `<body>` and the assertion that follows fails for a reason that has
 * nothing to do with what it is testing.
 */
async function fillMessage(value = 'Ship the thing') {
  const field = screen.getByLabelText(TASK_LABEL);
  await userEvent.click(field);
  await userEvent.type(field, value);
}

/**
 * The collapsed Start from control.
 *
 * Matched on the label prefix, never on the whole string: the rest of the name
 * is the current choice, which is exactly what the assertions vary.
 */
function templateTrigger(): HTMLButtonElement {
  return screen.getByRole('button', { name: TEMPLATE_CHIP });
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

describe('NewTrackForm asks only what the track starts from', () => {
  /*
   * #1211 — a track starts unnamed, and this form asks for exactly one thing.
   *
   * The title is no longer the intent: the kernel takes `#[serde(default)]` for
   * it and the planner agent names the track once it knows what it is for. What is
   * collected instead is the sentence, and Create is gated on **it** and on
   * nothing else — so this pair is the whole gate, in both directions.
   *
   * S2 stated this as "nothing is required, Create is live on first paint",
   * which was true of a dialog that collected no sentence at all. S3 made the
   * composer the page: an empty one has nothing to submit, and a Create that
   * fired anyway would make a track with nothing in it and nothing on screen
   * saying why that was allowed.
   *
   * Red when a *second* required field comes back, whatever it is called (the
   * case below it goes green on the sentence alone), or when the sentence stops
   * gating the submit.
   */
  it('keeps submit disabled while the composer is empty', () => {
    renderForm();
    expect(submitButton().disabled).toBe(true);
  });

  it('enables submit on the sentence alone', async () => {
    renderForm();
    await fillMessage();
    expect(submitButton().disabled).toBe(false);
  });

  /*
   * #1299 — the sentence leaves this form **verbatim**.
   *
   * The composer's text is delivered to the planner agent by the create, and
   * the kernel forwards it untrimmed (`send_planner_input`) and hashes it
   * untrimmed (`first_message_digest`), so the whitespace around what the
   * reader typed is part of what they said. This form trims to decide whether
   * it may submit at all and for nothing else.
   *
   * The version of this case that shipped first asserted the *trimmed* string
   * and so certified the defect: the form trimmed, the route trimmed again,
   * and a deliberately indented instruction arrived flattened with every suite
   * green.
   */
  it('calls onSubmit with the sentence exactly as typed, whitespace and all', async () => {
    const { props } = renderForm();
    await fillMessage('  keep indentation  ');
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({ message: '  keep indentation  ' });
  });

  /*
   * Blank is the **kernel's** question, and this form must answer it the same
   * way (`isBlankForKernel`).
   *
   * The kernel refuses `text.trim().is_empty()` with Rust's `char::is_whitespace`
   * — the Unicode `White_Space` property — and JS `trim()` is a different set:
   * `U+0085 NEXT LINE` is whitespace to Rust and an ordinary character to JS.
   * A form gated on `trim()` therefore lights up Create for a `U+0085`-only
   * draft, posts it, and collects a 400 the reader cannot act on. Both gates
   * are asserted, because they are two call sites: the button's `disabled`
   * (`valid`) and the Enter path (`submit`'s own guard).
   */
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
   * What #1147 S3 changes, and why this is a restatement and not a deletion:
   * the dialog **does** ask for a folder now, optionally and empty by default.
   * The line that said `queryByLabelText('Folder')` is null is now the positive
   * assertion below — it was standing for "this dialog does not collect a
   * working directory", and that is no longer the truth: create time is the
   * only entry into an attached workspace, so the control has to be here. The
   * `Area` and `Claim this folder` absences are untouched — the area is the
   * opener's, and the claim is implied by picking a folder (see `app/shell`).
   */
  it('asks for a template and an optional folder — never an area or claim control', async () => {
    const { props } = renderForm();
    await fillMessage();
    // Present, and empty: the placeholder is what an unset folder shows, and an
    // unset folder is the managed default.
    /* Unset, the chip names the **default** — no row, paragraph or label
       around it. What it does *not* hold is a path, which is the only thing
       that could be a value. Its name and hover string say which control it is
       on top of that, because the bare text does not survive being read on its
       own. */
    expect(folderChip().textContent).toBe(FOLDER_PLACEHOLDER);
    expect(folderChip().getAttribute('title')).toBe(FOLDER_CHIP_NAME);
    /* And nothing explains the mechanism under the row: two chips that name
       what they hold is the whole of it. Matched on the mechanism's own words
       rather than on /workspace/i — the chip itself now says "Neige
       workspace", so the looser pattern would match the control it exists to
       check is unaccompanied. */
    expect(screen.queryByText(/allocates|git init|managed workspace/i)).toBeNull();
    // Empty means nothing was read: the picker only reaches its port on open.
    expect(props.listDirectory).not.toHaveBeenCalled();
    expect(screen.queryByLabelText('Area')).toBeNull();
    // The folder is not the legacy "Working directory" field either: that one
    // was a free-text path with a claim checkbox beside it.
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
    /* #1292 — the last row is the way to the recipe editor, and it is not a
       template: it is present whether or not the reader has recipes, because
       it is the only entry point to writing one. With no recipes there are no
       band headings either; `recipe-picker.test.tsx` asserts that half. */
    expect(screen.getAllByRole('menuitem').map((item) => item.textContent))
      .toEqual(['No templateSelected', 'Issue development', 'Small change', 'Investigation', 'Manage recipes…']);
  });

  /*
   * Single line, not a textarea, and named without a visible label row. The
   * value becomes the track's `title`, and every other surface renders it as
   * one truncated line — sidebar, track list, page header — while the track page
   * edits it through the single-line `EditableTitle`.
   * `getByRole('textbox')` is true of both elements, so this asserts the tag
   * itself; anything weaker would stay green on a textarea.
   */
  /*
   * #1211 — the field is a composer, not a one-line input.
   *
   * It was `<input type="text">` because its value was the track's `title`, and
   * every surface that shows a title renders one truncated line. The value is
   * now the track's *intent*, delivered as the first message to the planner card,
   * and an intent is a sentence: the field is astryx's `contenteditable`, it
   * wraps, and Shift+Enter adds a line.
   *
   * `aria-multiline` is the assertion that would catch a silent regression to
   * a single-line control, which is what `tagName === 'INPUT'` used to do.
   */
  it('asks for the task in a multi-line composer, named but with no label row', () => {
    renderForm();
    const task = screen.getByLabelText(TASK_LABEL);
    expect(task.getAttribute('contenteditable')).toBe('true');
    expect(task.getAttribute('aria-multiline')).toBe('true');
    // The row the user asked us to reclaim: the prompt lives in the box.
    expect(screen.getByText(TASK_PLACEHOLDER)).toBeTruthy();
  });

  it('flips the label and blocks submit while submitting', () => {
    renderForm({ submitting: true });
    expect(screen.getByRole('button', { name: 'Creating…' })).toHaveProperty('disabled', true);
  });

  it('surfaces the caller error in an alert region', () => {
    renderForm({ error: 'Could not create the track.' });
    expect(screen.getByRole('alert').textContent).toContain('Could not create');
  });

  /*
   * #1211 — there is no Cancel, and its absence is the assertion.
   *
   * The surface is a route: the way out is Back, and a button that means Back
   * without touching history is a second, wrong exit. This replaces a test that
   * clicked Cancel and checked `onCancel`; deleting it outright would have left
   * nothing saying the button is *meant* to be gone, so the next reader adding
   * one back would meet a green suite.
   */
  /*
   * Enter must not throw the sentence away when the submit is refused.
   *
   * astryx's `ChatComposer.handleSubmit` is `onSubmit(trimmed); updateValue('')`
   * — it clears the controlled value **unconditionally and synchronously after**
   * calling us, and our `submit` returns early whenever the draft is not
   * submittable. So the refusal path used to be: the text vanishes, nothing is
   * created, and nothing is said.
   *
   * Driven through the bound template with no issue URL, which is the honest
   * reproduction: `inputBlocker` is true, the send button is disabled, and the
   * reader has every reason to think Enter is safe to press.
   */
  it('keeps the draft when Enter is pressed on a submit the form refuses', async () => {
    const { onSubmit } = renderForm();
    await fillMessage('Ship the thing');
    await userEvent.click(templateTrigger());
    await userEvent.click(screen.getByRole('menuitem', { name: /^Issue development/ }));
    // The blocked state the reader is in: no issue URL yet.
    expect(submitButton().disabled).toBe(true);

    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.keyboard('{Enter}');

    expect(onSubmit).not.toHaveBeenCalled();
    expect(screen.getByLabelText(TASK_LABEL).textContent).toBe('Ship the thing');
  });

  /* The other half of owning Enter: it still creates. Without this, the fix
     for the case above ("stop Enter reaching astryx") would pass by disabling
     Enter outright, which is the same defect wearing a different hat. */
  it('creates on Enter when the draft is submittable', async () => {
    const { onSubmit } = renderForm();
    await fillMessage('Ship the thing');
    await userEvent.keyboard('{Enter}');
    expect(onSubmit).toHaveBeenCalledWith({ message: 'Ship the thing' });
  });

  /* Shift+Enter is a newline, not a submit — the composer is multi-line, and
     that is the only way to reach the second line. */
  it('does not create on Shift+Enter', async () => {
    const { onSubmit } = renderForm();
    await fillMessage('Ship the thing');
    await userEvent.keyboard('{Shift>}{Enter}{/Shift}');
    expect(onSubmit).not.toHaveBeenCalled();
  });

  /*
   * Enter belongs to the field, not to the whole composer.
   *
   * The first cut of "own Enter" put the capture handler on the composer
   * wrapper, which contains the footer chips *and* astryx's menu layer (its
   * popover does not portal). Capturing there swallowed Enter for every control
   * inside: the chips would not open, and — worst — arrowing to a template in
   * the open menu and pressing Enter created a track with **no** template
   * instead of selecting one, then navigated away from it.
   */
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

  /*
   * The subtler half of "Enter is the field's": there are focusable controls
   * *inside* the editable. astryx turns any paste over 200 characters into a
   * token whose hover card carries an `Expand` button, and that button is a DOM
   * descendant of the `contenteditable` — so a `closest()` check captured its
   * Enter and created a track instead of expanding the token. Pasting a long
   * instruction here is entirely ordinary.
   *
   * The control is planted directly rather than driven through a real paste:
   * what is under test is the guard's *target rule*, and jsdom's clipboard path
   * would be testing astryx's tokeniser instead. Planting it is also the
   * stricter case — it asserts the rule for any descendant control, not only
   * the one shape astryx happens to render today.
   */
  it('leaves Enter to a control inside the field', async () => {
    const { onSubmit } = renderForm();
    await fillMessage('Ship the thing');
    const field = screen.getByLabelText(TASK_LABEL);

    const expand = document.createElement('button');
    expand.textContent = 'Expand';
    /* `contenteditable="false"`, which is how a real embedded control is
       mounted inside an editable — astryx sets exactly this on its token nodes
       (`ChatComposerInput.tsx`). Without it the editable keeps focus and the
       keydown targets the editable, so the case would pass for the wrong
       reason: it would never exercise the descendant at all. */
    expand.setAttribute('contenteditable', 'false');
    field.appendChild(expand);
    let clicks = 0;
    expand.addEventListener('click', () => { clicks += 1; });
    expand.focus();
    expect(document.activeElement).toBe(expand);
    await userEvent.keyboard('{Enter}');

    expect(onSubmit).not.toHaveBeenCalled();
    /* And the control still works. Both halves are needed: `stopPropagation`
       alone stops the track being created, and adding `preventDefault` alongside
       it would kill native button activation and leave `Expand` permanently
       dead — the other half of the original defect, which the first version of
       this test could not see. Review caught that by mutation; this line is
       what makes it visible here. */
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

  /*
   * #1299 — the composer no longer warns that the sentence will have to be
   * repeated, because it no longer will be: the route delivers it on the create
   * (`first_message`). The warning went, and so did the `aria-describedby` that
   * pointed at it — a description is a promise about what happens to what you
   * type, and there is no longer one to make that the label does not already.
   *
   * Asserted as an absence in both channels, because the two failed
   * differently: the visible string could come back on its own, and the
   * attribute could be left behind pointing at an id nothing renders, which is
   * a dangling IDREF a sighted reviewer cannot see.
   */
  it('promises no repetition, and leaves no description pointing at nothing', () => {
    renderForm();
    expect(screen.queryByText("You'll say this again in the track's chat")).toBeNull();
    const describedBy = screen.getByLabelText(TASK_LABEL).getAttribute('aria-describedby');
    expect(describedBy === null || document.getElementById(describedBy) !== null).toBe(true);
  });

  /*
   * The IME half of owning Enter: while a candidate is being composed, Enter
   * *accepts the candidate* and must not create a track. Removing the
   * `isComposing` guard leaves every other Enter case green, so this is the
   * only thing standing between a CJK reader and a track created mid-word.
   *
   * Driven with a real `compositionstart` and an `isComposing` keydown, because
   * `userEvent.keyboard` cannot set that flag.
   */
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
    /* The collapsed trigger *is* the answer to "what is selected" — there is
       no checked radio left to read it off. Its accessible name is the control's
       word plus the current choice, which is also why a `<label htmlFor>`
       cannot be used here: it would replace the choice with the label.
       Since #1211 the unset chip names the default rather than asking, so the
       name to expect on open is the default's, not a question. */
    expect(screen.getByRole('button', { name: 'Template: No template' })).toBe(templateTrigger());
    await fillMessage();
    await userEvent.click(submitButton());
    const [draft] = onSubmit.mock.calls[0] as [Record<string, unknown>];
    expect(draft).toEqual({ message: 'Ship the thing' });
    // Not `null`, not `''` — the kernel 400s a whitespace-only id and the body
    // is `deny_unknown_fields`. Absence is the only spelling of "no template".
    expect(Object.hasOwn(draft, 'template_id')).toBe(false);
  });

  /*
   * The failure mode this guards is the real one: `GET /api/track-templates` is
   * down or slow, and the app's only track-creation entry point becomes a
   * dialog that cannot create a track. An empty list is what a pending or
   * failed read looks like from here.
   */
  it('still creates a track when the template read gave nothing', async () => {
    const { props } = renderForm({ templates: [], templatesError: 'Could not load templates.' });
    expect(screen.getByRole('button', { name: 'Template: No template' })).toBe(templateTrigger());
    await openTemplates();
    /* Two: "No template", still the working default, and "Manage recipes…",
       which does not create anything. Neither is a template — the read gave
       none — so this is still the "the picker offers nothing but the free
       choice" assertion it was before #1292. */
    expect(screen.getAllByRole('menuitem').map((item) => item.textContent))
      .toEqual(['No templateSelected', 'Manage recipes…']);
    await userEvent.keyboard('{Escape}');
    await fillMessage();
    expect(submitButton().disabled).toBe(false);
    await userEvent.click(submitButton());
    expect(props.onSubmit).toHaveBeenCalledWith({ message: 'Ship the thing' });
  });

  it('says the templates are missing without claiming the create failed', () => {
    renderForm({ templates: [], templatesError: 'Could not load templates.' });
    // A `status`, not an `alert`: nothing the user did failed.
    expect(screen.queryByRole('alert')).toBeNull();
    expect(screen.getByText(/Could not load templates\..*still create a track without one/)).toBeTruthy();
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
  async function chooseIssueDev() {
    await fillMessage();
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
      message: 'Ship the thing',
      template_id: 'issue-development',
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
   * would reject `template_input`. The picker must follow: still offerable
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
      message: 'Ship the thing', template_id: 'issue-development',
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
    await fillMessage();
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
    expect(within(menu).getByRole('menuitem', { name: /^No template/ }).getAttribute('aria-describedby'))
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
   * Green when: the dialog's tab order is picker → folder → actions, and
   * every option carries `tabindex="-1"`.
   * Red when: the "N tasks" trigger (or any other `tabindex="0"`) comes back
   * inside the menu, or an option is left tabbable.
   *
   * The **folder** stop is #1147 S3's, and the task stop that used to open the
   * walk is gone with the field (#1211 S2), so the walk is three steps again —
   * and it now *starts* at the picker, which is also where the dialog puts its
   * opening focus. The picker is still exactly one stop, which is what the
   * case exists to pin. The folder's "Use a Neige workspace instead" companion
   * is deliberately not in the walk: it does not exist until a folder has been
   * chosen, and none has here.
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
    await fillMessage();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    const order: Element[] = [];
    for (let step = 0; step < 3; step += 1) {
      await userEvent.tab();
      if (document.activeElement !== null) order.push(document.activeElement);
    }
    /* Three stops, not four: Cancel is gone (#1211 — leaving a route is Back).
       The fourth tab therefore leaves the composer entirely, which is why the
       loop still takes four steps and the assertion takes the first three. */
    expect(order.slice(0, 3)).toEqual([
      templateTrigger(),
      folderChip(),
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
    expect(document.activeElement?.textContent).toContain('No template');
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

/*
 * #1147 S3 — the folder, and the two request shapes it decides.
 *
 * The kernel keys its *managed*-workspace branch on the **absence** of `cwd`
 * (`routes/tracks.rs`'s `cwd_omitted`): it allocates a directory, `git init`s
 * it and owns it. A path plus `attach_folder` takes the *attached* branch
 * instead, against a repository the user already has. Create time is the only
 * entry into that second branch — `managed → attached` after the fact is an
 * API with no UI — so all of the following is reachable from nowhere else.
 */
describe('The folder is optional, and its absence is the managed default', () => {
  it('never requires a folder to submit', async () => {
    const { onSubmit } = renderForm();
    expect(submitButton().disabled).toBe(true);
    await fillMessage();
    // The title alone enables it: nothing about the folder gates the submit,
    // before or after the picker has been opened.
    expect(submitButton().disabled).toBe(false);
    await userEvent.click(folderChip());
    await screen.findByDisplayValue('/srv/app/');
    await userEvent.keyboard('{Escape}');
    await userEvent.click(submitButton());
    const [draft] = onSubmit.mock.calls[0] as [Record<string, unknown>];
    expect(Object.hasOwn(draft, 'cwd')).toBe(false);
  });

  /*
   * The default, asserted on *key absence* and not on a value. `cwd: ''` and
   * `cwd: undefined` both take the attached branch for anything that inspects
   * the draft before serialization, and `''` is a path that cannot work.
   * `toEqual` is what pins the absence; `toMatchObject` stays green on an
   * extra key.
   */
  it('submits no cwd key at all when no folder was chosen', async () => {
    const { onSubmit } = renderForm();
    await fillMessage('  Ship the thing  ');
    await userEvent.click(submitButton());
    const [draft] = onSubmit.mock.calls[0] as [Record<string, unknown>];
    /* Padded on purpose, and the padding survives: this case is about the
       absent `cwd`, but it types whitespace, so it would silently re-certify
       a trimming `message` if it asserted the trimmed string. */
    expect(draft).toEqual({ message: '  Ship the thing  ' });
    expect(Object.hasOwn(draft, 'cwd')).toBe(false);
  });

  /*
   * CAP-TRACKWORKSPACE-003, and the regression it was rewritten for (#1211).
   *
   * `DirectoryField` decides how to open the picker by asking `useDialogView()`
   * whether a dialog is above it: inside one it pushes a child view, outside
   * one it falls back to rendering `DirectoryBrowser` **inline in the page**.
   * Moving this form from a dialog onto a route silently took that fallback,
   * and the picker became a file list unrolled under the chip — no focus trap,
   * no Escape, no click-outside.
   *
   * So the assertion is on the *modal*, not on which component opened it: the
   * browser must be inside an `aria-modal` dialog, and nothing may render it in
   * the page. An assertion naming `DirectoryField` could not have caught the
   * regression, because the control was still `DirectoryField`.
   */
  it('opens the picker in a modal dialog, never inline in the page', async () => {
    renderForm();
    expect(screen.queryByRole('dialog')).toBeNull();
    await userEvent.click(folderChip());
    const picker = await screen.findByRole('dialog');
    expect(picker.getAttribute('aria-modal')).toBe('true');
    expect(picker.getAttribute('aria-label')).toBe('Choose a directory');
    // The browser is *inside* it — not a sibling left in the page behind it.
    await within(picker).findByDisplayValue('/srv/app/');
    expect(within(picker).getByRole('button', { name: 'Select this directory' })).toBeTruthy();

    /* And it is the *only* one. Without this the assertions above pass while a
       second browser is also unrolled inline in the page — the modal exists, so
       "the browser is inside a modal" is satisfied, and the defect this case
       exists to catch is sitting next to it.

       `{ hidden: true }` is load-bearing, and the first cut of this line did
       not have it. `Dialog` marks everything outside the portal `inert` +
       `aria-hidden` while it is open, and Testing Library's role queries skip
       that subtree by default — so a second, inline browser was invisible to
       the count and the assertion passed with the defect on screen. Verified by
       rendering one alongside: without `hidden` this case stays green, with it
       the count is 2 and it fails. */
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

  /* Create time is the only entry into the attached choice, so the way *back*
     to the default has to exist here too — there is no later screen for it. */
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

  /*
   * The picker reads through the injected port and never a transport of its
   * own: `ui/` primitives may not know a transport exists, and `features/**`
   * may not import `app/**`, so the only route to the filesystem is the prop.
   */
  it('reads the directory through the injected port', async () => {
    const { props } = renderForm();
    await userEvent.click(folderChip());
    await screen.findByDisplayValue('/srv/app/');
    expect(props.listDirectory).toHaveBeenCalled();
  });

  /*
   * The folder rides alongside #1209's template rather than replacing it: the
   * two are collected by different controls and merged into one draft, and
   * nothing else in this file proves the merge keeps both.
   */
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
