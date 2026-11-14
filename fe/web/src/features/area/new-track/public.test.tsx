// @vitest-environment jsdom
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { createStaticSource } from '@astryxdesign/core/Typeahead';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { DirectoryListing } from '../../../ui/directory-browser/public.tsx';
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
const FOLDER_CHIP_NAME = `Repo: ${FOLDER_PLACEHOLDER}`;


function renderForm(overrides: Partial<Parameters<typeof NewTrackForm>[0]> = {}) {
  const onSubmit = vi.fn();
  const props = {
    submitting: false,
    error: null,
    initialCwd: null,
    /* Required, so a call site that forgot it cannot render a dead menu row. */
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
    const { onSubmit } = renderForm({ submitBlocked: true });
    await fillMessage('Ship the thing');
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
    folderChip().focus();
    await userEvent.keyboard('{Enter}');
    expect(await screen.findByRole('dialog', { name: 'Choose a directory' })).toBeTruthy();
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
    expect(onSubmit).toHaveBeenCalledWith({ message: 'Ship the thing', cwd: '/srv/app', isolation: 'none' });
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
});
