// The new-wave form: a task, what the wave starts from, and optionally the
// folder it runs in.
//
// Presentational + local form state — it never calls an API. The caller owns
// POST /api/waves, `submitting`, `error`, and the template list itself.
//
// `cove_id` is not a form field. The dialog opens from a cove page `+` (or the
// rail's per-cove `+`); the caller already knows which cove and sends it on
// the request.
//
// ## The folder is optional and empty by default (#1147 S3)
//
// Left empty, the draft carries no `cwd` at all and the caller's POST omits
// `cwd` / `attach_folder`, which is the kernel's *managed*-workspace branch: it
// allocates a directory under the workspace root, `git init`s it, and owns it.
// Filled in, the wave is *attached* to a repository the user already has, which
// the kernel never creates, moves or deletes. Create time is the only entry
// into that choice — `managed → attached` after the fact exists as an API and
// has no UI — so an always-visible optional field is the whole feature, not a
// shortcut for one. Without it, attached workspaces are unreachable from the
// product.
//
// ## Built from `@astryxdesign/core`
//
// The first cut of this form hand-rolled native radios and a CSS module for
// the row card. That was wrong: astryx is this repo's component library
// (`fe/README.md`), the stylesheet cascade already reserves a layer for it
// (`styles/README.md`), and it ships every control this form needs.
//
// The outer `Dialog` is deliberately NOT astryx's: `ui/dialog/public.tsx` is a
// frozen primitive whose nine global classes are a closed list, so swapping it
// is a spec change and its own slice. Only the form's insides are astryx.
//
// ## The template (#1209)
//
// "No template" is a first-class option and the default, and it is **not** a
// row the server sent: it is the absence of a template, i.e. a create with no
// `workflow_id` — precisely today's behaviour. Everything about this list is
// arranged so that staying on it is free. In particular `templates` may be
// empty because the read failed or has not landed, and the dialog is fully
// usable in that state: this is the app's only wave-creation entry point, and
// a failed list read must not be able to close it.
//
// The words on screen are the reader's, not the codebase's. The chip asks
// ("Choose a template") until there is something to name, and the sentinel is
// "No template" in the list — `Blank` was this file's own word for "no
// `workflow_id` on the wire", and it had ended up on a control read by someone
// who has not been told this app has workflows.
//
// The vocabulary seam is deliberate and recorded in #1209: the read side says
// *template*, the write side says `workflow_id`. This form speaks the read
// side's word to the user and the write side's word on the wire.
//
// ### Collapsed, not spread out
//
// The first cut laid every template out as a permanent radio row. Three rows
// today; the list is meant to grow, and a dialog that grows a row per template
// does not scale. So the control collapses to one row that names the current
// choice and opens a list on click.
//
// `DropdownMenu` and not `Selector`, `Popover` or `CommandPalette` — the
// reason is where DOM focus goes, and it decides the hover card below:
//
//   * `Selector` is the semantically nicer control (`role="listbox"` +
//     `aria-selected`, which is exactly "one of N"), but it drives its list
//     with `aria-activedescendant`: DOM focus never leaves the trigger button
//     (`Selector.tsx` keeps `triggerRef` focused and only sets
//     `aria-activedescendant`). An option therefore never receives `focusin`,
//     so a per-option hover card would be mouse-only — the very defect this
//     revision exists to fix. It also renders `role="combobox"`.
//   * `DropdownMenu` navigates by *moving focus*: `useListFocus.focusIndex`
//     calls `target.focus()` on the `[role="menuitem"]` element. That is what
//     makes a hover card attached to the option itself reachable by keyboard.
//     Its items are `tabIndex={-1}`, so the whole control is one tab stop.
//   * `Popover` is an empty surface — using it means hand-rolling the list,
//     its roles and its keyboard model, which is what astryx is here to avoid.
//   * `CommandPalette` is a modal search dialog. A second modal inside the New
//     wave dialog, with a search box, for three options.
//   * `Selector` **with `renderOption`** — the fifth shape, and the one the
//     first write-up of this comparison missed. `Selector` takes
//     `renderOption?: (option: SelectorOptionData) => ReactNode`
//     (`Selector/Selector.tsx`), and `SelectorOption` takes
//     `description?: ReactNode` (`Selector/SelectorOption.tsx`). Rendering the
//     template's task keys as a one-line description therefore puts the same
//     information *inside* `role="option"`, and it needs no hover card at all:
//     no `focusTrigger` workaround, no top layer, and `aria-selected` says
//     "one of N" outright — so neither of the two stand-ins below would exist.
//     (`SelectorOptionData` itself has no `description` field; the review that
//     raised this read the prop off the component. The route is real, the
//     field is not — it goes through `renderOption`.)
//     Not taken here, and not because it cannot be done: this form's shape is
//     already accepted, the description would land inside the option's
//     accessible name (an option would read "Small change inspect, implement,
//     verify"), and the multi-line goal text the card shows does not fit a
//     one-line description. Recorded so the next reader does not conclude that
//     astryx has no listbox answer — it does, and it is the cheaper one if the
//     content is ever cut down to keys.
//
// The one thing `DropdownMenu` cannot express is *which* item is chosen:
// `DropdownMenuItem` hard-codes `role="menuitem"` and offers no
// `menuitemradio`/`aria-checked`. Two things stand in for it, and both are
// asserted: the trigger's accessible name is "Template: <current choice>" —
// an `aria-label`, because the chip's *text* is the bare choice and a name
// read on its own has to say which kind of choice it is — and the chosen item
// carries a check icon plus a `VisuallyHidden` "Selected".
//
// ### Two astryx limits this shape runs into, measured and left standing
//
// Neither is fixable from here — both live inside `@astryxdesign/core` — so
// they are written down rather than worked around with a local fork.
//
//  1. **The menu's accessible name is the current selection, not "Template".**
//     `DropdownMenu` names its popup from its trigger's *label*
//     (`aria-label={button.label}`, `DropdownMenu.tsx`) and not from the
//     trigger's computed name, so the `aria-label` that makes the trigger read
//     "Template: Small change" does not reach the popup: a reader who opens it
//     on a chosen template hears "Small change menu". Unset the two coincide,
//     because the label is then "Choose a template" outright. The alternative
//     is worse: dropping the choice out of the visible label would make the
//     collapsed control silent about what is selected, which is the whole
//     reason it carries it.
//  2. **The hover card's `role="dialog"` is a DOM descendant of the
//     `role="menu"`.** `HoverCard` renders its layer inline next to the
//     trigger — deliberately, "no portal is needed" (`HoverCard.tsx`), because
//     the Popover API's top layer plus CSS anchor positioning already escape
//     clipping. The trigger here is a menu item, so the layer is emitted
//     inside the menu, and a `menu`'s owned children are supposed to be
//     `menuitem`s only. In Chromium the computed tree is very likely still
//     correct — the intervening wrapper is `display: contents` with no role,
//     and the popover is in the top layer — but that is astryx's rendering
//     detail carrying the ARIA structure, not something this file guarantees.

import { useId, type RefObject } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { CheckboxInput } from '@astryxdesign/core/CheckboxInput';
import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';
import { FieldStatus } from '@astryxdesign/core/FieldStatus';
import { HoverCard } from '@astryxdesign/core/HoverCard';
import { HStack } from '@astryxdesign/core/HStack';
import { Icon } from '@astryxdesign/core/Icon';
import { List, ListItem } from '@astryxdesign/core/List';
import { TextInput } from '@astryxdesign/core/TextInput';
import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';
import { VStack } from '@astryxdesign/core/VStack';

import { parseGitHubIssueUrl } from '../../../../../core/domain/issue-url.ts';
import type { WaveTemplate } from '../../../../../core/domain/wave.ts';
import type { ListDirectory } from '../../../ui/directory-browser/public.tsx';
import { DirectoryField } from '../../../ui/schema-form/fields/DirectoryField/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import styles from './new-wave.module.css';

export type NewWaveDraft = Readonly<{
  title: string;
  /** Absent for Blank — never `null` or `''`, which the kernel 400s. */
  workflow_id?: string;
  workflow_input?: Readonly<Record<string, unknown>>;
  /**
   * Absolute path, **or the key is absent**. Absent is not "the empty string":
   * the caller distinguishes the two to decide whether the request carries
   * `cwd` / `attach_folder` at all, and an empty string is a legal-looking
   * value that would take the attached branch with a path that cannot work.
   */
  cwd?: string;
}>;

export type NewWaveFormProps = Readonly<{
  submitting: boolean;
  error: string | null;
  /**
   * Templates the user may start from, from `GET /api/wave-templates`. An
   * empty array is a legitimate, fully working state — Blank only.
   */
  templates: readonly WaveTemplate[];
  /**
   * Set when the template read failed. It is a *notice*, not an error: the
   * form still submits. Told rather than hidden, so "where did my templates
   * go" has an answer on screen.
   */
  templatesError?: string | null;
  /**
   * The folder picker's read port. Injected: `ui/` primitives never reach a
   * transport, and `features/**` may not import `app/**` — so the port is
   * created at the composition layer (`app/providers/directory.ts`) and passed
   * down. Required, not optional: a call site that forgot it would render a
   * picker that silently lists nothing.
   */
  listDirectory: ListDirectory;
  /*
   * The dialog's opening focus target. Without one the dialog falls back to its
   * first focusable, which is the header's Close button — so a reader who
   * opened this and started typing put nothing in the field and closed the
   * dialog on the first space. See #1161.
   *
   * Required rather than optional: the defect was a call site that simply did
   * not think about opening focus, and an optional prop lets the next one make
   * the same omission silently.
   *
   * astryx's `TextInput` forwards its ref to the `<input>` itself, so this
   * keeps pointing at the element the dialog must focus.
   */
  titleRef: RefObject<HTMLInputElement | null>;
  onCancel: () => void;
  onSubmit: (draft: NewWaveDraft) => void;
}>;

/** The one template whose inputs this form knows how to collect. */
const ISSUE_DEVELOPMENT = 'issue-development';

/**
 * Selection sentinel for Blank.
 *
 * `''` because Blank is the *absence* of a template id, which no server row
 * can ever collide with, and because that absence is what goes on the wire.
 */
const BLANK = '';

/**
 * The two things the template chip can say, and neither of them is "Blank".
 *
 * `Blank` was the codebase's word for "no `workflow_id` on the wire", and it
 * had leaked onto a chip a person reads before they know this app has
 * workflows at all. What a reader needs from an unset control is what it is
 * *for*, in the words they would use themselves — so unset the chip asks, and
 * once a choice exists it names it. The absence itself keeps a plain name of
 * its own in the menu, because a list of alternatives has to be able to offer
 * "none" as one of them.
 */
const CHOOSE_TEMPLATE = 'Choose a template';
const NO_TEMPLATE = 'No template';

/**
 * The Task field's accessible name.
 *
 * Not rendered: the field is one line and a label above it spent a whole row
 * to say what the placeholder already says. Hidden, not absent — an unnamed
 * textbox is unusable by screen reader and by voice control alike.
 *
 * It is also no longer "Task". This value becomes the wave's `title`, and
 * calling it Task was a second name for a field that already had one.
 */
const TASK_LABEL = 'What this wave should do';
const TASK_PLACEHOLDER = 'What should this wave do?';

/**
 * What the folder chip says while there is no folder — its visible text, its
 * accessible name and its hover string at once.
 *
 * It is the same sentence shape as the template chip beside it on purpose: two
 * controls that do the same kind of thing should ask the same kind of
 * question, and "Choose a …" is the shortest form of the only thing a reader
 * needs from either — what tapping it is going to do.
 *
 * What used to sit under this row was a sentence about what Neige does when
 * the folder is left alone (it allocates a workspace and `git init`s it). That
 * is true, and it is the implementation talking: it explains a mechanism to
 * someone who has not yet been told there is a choice. The choice is what the
 * chip now says; the mechanism is not something the reader has to hold to make
 * it. The managed-vs-attached distinction is still exactly one click deep —
 * the picker is where a folder gets chosen — and it stays in
 * `DirectoryField`'s and the kernel's own documentation.
 */
const FOLDER_PLACEHOLDER = 'Choose a folder';
/** The way back to the managed default, which exists nowhere else. */
const FOLDER_CLEAR_LABEL = 'Use a Neige workspace instead';

/** Mirrors the enum in the bound plugin's `input_schema`. */
type MergePolicy = 'hold-for-ratify' | 'auto-merge';

/**
 * A template takes input iff a running trusted plugin is bound to it, which is
 * exactly when the read returned an `input_schema`. Branching on that instead
 * of on the id keeps this in step with what the create path will accept: with
 * the plugin stopped, `issue-development` still seeds its report and must be
 * offered — just without the fields the kernel would then reject.
 */
function needsInput(template: WaveTemplate | undefined): boolean {
  return template?.input_schema != null;
}

export function NewWaveForm({
  submitting, error, templates, templatesError = null, listDirectory,
  titleRef, onCancel, onSubmit,
}: NewWaveFormProps) {
  const fieldId = useId();
  const [title, setTitle] = useState('');
  const [selected, setSelected] = useState<string>(BLANK);
  const [issueUrl, setIssueUrl] = useState('');
  const [autoMerge, setAutoMerge] = useState(false);
  const [cwd, setCwd] = useState('');
  const folderId = `${fieldId}-folder`;
  const triggerId = `${fieldId}-start-from-trigger`;
  const startFromStatusId = `${fieldId}-start-from-status`;

  // A template that vanished between renders (the list refetched without it)
  // must not leave a selection pointing at nothing; falling back to Blank is
  // the safe direction — it always submits.
  const chosen = templates.find((template) => template.id === selected);
  const effectiveSelection = selected === BLANK || chosen ? selected : BLANK;
  const wantsInput = needsInput(chosen);
  const issueDev = wantsInput && effectiveSelection === ISSUE_DEVELOPMENT;
  const parsedIssue = issueDev ? parseGitHubIssueUrl(issueUrl) : null;

  // Fail-closed: a bound template this build has no editor for cannot be
  // submitted, because the kernel requires the input its schema declares and
  // guessing at it would trade a readable block for a 400.
  const unsupportedInput = wantsInput && !issueDev;
  const issueUrlTouched = issueUrl.trim() !== '';
  const issueUrlBad = issueDev && issueUrlTouched && parsedIssue === null;
  const inputBlocker = unsupportedInput || (issueDev && parsedIssue === null);
  const valid = title.trim() !== '' && !inputBlocker;

  /*
   * One status slot on the field, and the two things that can fill it never
   * coexist: `templatesError` means the list is empty, and an empty list has
   * no bound template to be unsupported. Error vs warning is the difference
   * that matters to a reader — one blocks the submit, the other does not.
   */
  const groupStatus = unsupportedInput
    ? { type: 'error' as const, message: 'This template needs input this version cannot collect yet.' }
    : templatesError !== null
      ? { type: 'warning' as const, message: `${templatesError} You can still create a blank wave.` }
      : undefined;

  function buildDraft(): NewWaveDraft {
    /* Spread, not `cwd: folder || undefined`: the caller keys the whole
       managed-vs-attached decision on whether the key is *there*, and
       `cwd: undefined` is a different object from no `cwd` for anything that
       inspects the draft before it is serialized — including the tests. */
    const folder = cwd.trim();
    const base = { title: title.trim(), ...(folder === '' ? {} : { cwd: folder }) };
    if (effectiveSelection === BLANK) return base;
    if (parsedIssue === null) return { ...base, workflow_id: effectiveSelection };
    // The kernel applies no schema defaults, so `merge_policy` always travels
    // explicitly. Unchecked is `hold-for-ratify`: the default direction is
    // "wait for a human", and flipping it would auto-merge by omission.
    const mergePolicy: MergePolicy = autoMerge ? 'auto-merge' : 'hold-for-ratify';
    return {
      ...base,
      workflow_id: effectiveSelection,
      workflow_input: { ...parsedIssue, merge_policy: mergePolicy },
    };
  }

  return (
    /* `VStack as="form"`, not a hand-rolled flex column: the vertical rhythm
       is astryx's `gap` step (2 = 8px, the `--space-4` this used to restate).
       `styles.form` is what astryx does not own — the dialog's text colour and
       font, which come from this app's tokens. */
    <VStack
      as="form"
      gap={2}
      className={styles.form}
      onSubmit={(event) => {
        event.preventDefault();
        if (!valid || submitting) return;
        onSubmit(buildDraft());
      }}
    >
      {error !== null && (
        <Banner status="error" title={error} data-nc-new-wave-error />
      )}

      {/* Single-line, not a textarea: this value is the wave's `title`, and
          every other place that shows it — sidebar, wave list, page header —
          renders it as one truncated line, and the wave page edits it through
          the single-line `EditableTitle`. A three-row box was this one entry
          point promising a shape the rest of the app cannot keep. */}
      <TextInput
        ref={titleRef}
        label={TASK_LABEL}
        isLabelHidden
        placeholder={TASK_PLACEHOLDER}
        value={title}
        width="100%"
        data-nc-new-wave-title
        onChange={(value) => setTitle(value)}
      />

      {/* ── The two settings, as one row of chips ────────────────────────────
          What this wave starts from and where it runs are the same *kind* of
          thing: one optional choice each, both defaulted, both changing only
          what the task above them is carried out on. They used to be two
          stacked full-width rows — a label, a box the width of the dialog, and
          for the folder a two-line paragraph under it — which gave two
          secondary settings more of the dialog than the sentence the wave is
          actually about. Same size, same variant, same row: the input is the
          dialog, and these sit under it the way a composer's controls sit
          under its text.

          Each chip says what it is for and then what it holds — "Choose a
          template" until one is chosen, then the template's title — so the row
          needs no labels above it and no paragraph under it. That is also why
          there is no `Field` around the trigger any more: a field exists to
          put a name beside a control, and these controls carry their own.
          What the `Field` did carry is the group's status message, and that is
          `FieldStatus` on its own, below the row. */}
      <HStack gap={1} align="center" className={styles.controls}>
        <DropdownMenu
          placement="below"
          button={{
            id: triggerId,
            label: chosen?.title ?? CHOOSE_TEMPLATE,
            /* The chip's text is the choice; its *name* has to survive being
               read on its own, out of the row, with nothing beside it — so it
               says which kind of choice it is. Unset the two coincide, because
               "Choose a template" already is that sentence. */
            'aria-label': chosen === undefined ? CHOOSE_TEMPLATE : `Template: ${chosen.title}`,
            variant: 'secondary',
            size: 'sm',
            className: styles.trigger,
            'aria-describedby': groupStatus !== undefined ? startFromStatusId : undefined,
          }}
        >
          {/* The absence of a template is an alternative like any other, and
              it is the one the dialog opens on. It carries no hover card
              because it has no tasks to show — its whole content is its name. */}
          <TemplateChoice
            label={NO_TEMPLATE}
            isSelected={effectiveSelection === BLANK}
            onSelect={() => setSelected(BLANK)}
          />
          {templates.map((template) => (
            <TemplateChoice
              key={template.id}
              label={template.title}
              tasks={template.tasks}
              isSelected={effectiveSelection === template.id}
              onSelect={() => setSelected(template.id)}
            />
          ))}
        </DropdownMenu>

        {/* The folder, #1147 S3, and no field around it: `DirectoryField` names
          itself (`aria-label` — its visible text is a basename, and a reader
          who hears only "app" learns nothing about which one), so a wrapping
          `<label>` here would be a second name for one control. It is also the
          frozen wrapper that pushes `DirectoryBrowser` into the *surrounding*
          dialog rather than opening a second one, which is why this form does
          not roll its own picker. */}
        <DirectoryField
          id={folderId}
          value={cwd}
          onChange={setCwd}
          listDirectory={listDirectory}
          placeholder={FOLDER_PLACEHOLDER}
        />

        {/* Create time is the only entry into the attached choice, so the way
            *back* to the managed default has to exist here too — there is no
            later screen for it. It appears beside the folder it undoes and only
            once there is one; icon-only, because a control that undoes a choice
            should not be wider than the choice. */}
        {cwd !== '' && (
          <Button
            type="button"
            variant="ghost"
            size="sm"
            isIconOnly
            icon={<Icon icon="close" size="sm" />}
            label={FOLDER_CLEAR_LABEL}
            onClick={() => setCwd('')}
          />
        )}
      </HStack>

      {/* The template group's one status slot, kept from the `Field` that used
          to own it. `detached` is the variant for a message under a control
          rather than overlapping an input's border, and the two things that
          can fill it never coexist — a failed list read leaves no bound
          template to be unsupported. */}
      {groupStatus !== undefined && (
        <FieldStatus
          id={startFromStatusId}
          type={groupStatus.type}
          message={groupStatus.message}
          variant="detached"
        />
      )}

      {issueDev && (
        /* Under the row that chose it, named by the template it belongs to.
           It is no longer inside the `Field` — that field is now one chip in a
           horizontal row and a panel cannot live in it — so the statement of
           belonging is adjacency plus the group's own accessible name, which
           is the template's title. The group needs a *name*, not a second
           visible heading: the trigger above already reads that title, and
           repeating it would be the same word twice in two rows. */
        <div className={styles.panel} role="group" aria-label={chosen?.title ?? ''}>
          <TextInput
            label="Issue URL"
            value={issueUrl}
            width="100%"
            placeholder="https://github.com/owner/repo/issues/123"
            /* An unfinished field is not an error: until something has been
               typed the guidance is a description, and only a value that
               cannot be parsed turns into `status` (which is what sets
               `aria-invalid` and the alert). */
            description={issueUrlBad ? undefined : parsedIssue === null
              ? 'Paste the GitHub issue this wave works on.'
              : `Issue #${parsedIssue.issue_number} in ${parsedIssue.repo}.`}
            status={issueUrlBad
              ? {
                type: 'error',
                message: 'Not a GitHub issue URL — expected https://github.com/owner/repo/issues/123.',
              }
              : undefined}
            onChange={(value) => setIssueUrl(value)}
          />
          <CheckboxInput
            label="Merge automatically once the gates converge"
            description="Off: the wave waits for you to approve the merge."
            value={autoMerge}
            onChange={(checked) => setAutoMerge(checked)}
          />
        </div>
      )}

      {/* The action row is a plain horizontal stack, so it is astryx's:
          `gap={1}` is 4px (the old `--space-2`) and `justify="end"` is the
          main-axis alias for `justify-content: flex-end`. */}
      <HStack gap={1} justify="end">
        <Button type="button" label="Cancel" variant="ghost" onClick={onCancel} />
        <Button
          type="submit"
          variant="primary"
          label={submitting ? 'Creating…' : 'Create wave'}
          isDisabled={submitting || !valid}
        />
      </HStack>
    </VStack>
  );
}

/**
 * One alternative in the Start from menu, and — when it has tasks — the
 * "what will this give me" card behind it.
 *
 * ## The row is the trigger
 *
 * The previous cut hung the card off a separate "N tasks" label in the row's
 * `endContent`. `HoverCard` renders a string child as a focusable
 * `<span tabIndex={0}>`, so that label was a *second* tab stop inside every
 * row — a composite control is supposed to be one stop, and a test had been
 * written that asserted the extra stop existed, fixing the defect in place.
 *
 * The card now hangs off the option itself. It costs no tab stop, because
 * `DropdownMenuItem` renders `tabIndex={-1}`: the menu is entered from its
 * trigger and walked with arrow keys, and `useListFocus.focusIndex` moves real
 * DOM focus onto the `[role="menuitem"]` element. `focusin` on the option is
 * therefore what opens the card — the keyboard gets the same affordance as the
 * pointer, on the same element, with no stop of its own.
 *
 * `focusTrigger="always"` is load-bearing and not defensive: the default
 * `'auto'` attaches focus listeners only when `useHoverCard`'s `isFocusable`
 * says so, and that helper returns `false` for an element with `tabindex="-1"`
 * — which is every menu item here. Left at `'auto'` the card would be
 * hover-only, i.e. the original defect with a new shape.
 *
 * ## The one sharp edge, reported as measured
 *
 * `useHoverCard` attaches a *native* `keydown` listener to its trigger that
 * calls `stopPropagation()` on Escape. The trigger here is the menu item, so
 * that listener sits below `DropdownMenu`'s React `onKeyDown` — which is
 * delegated at the root and therefore never runs. Escape's effect on the
 * *menu* is thereby handed to the engine's close request against
 * `popover="auto"`, and which layer that request lands on turns on whether the
 * DOM listener already hid the card: measured in Chromium it goes both ways
 * between runs, and in jsdom the menu never closes at all.
 *
 * The card itself always closes on Escape, and the menu always closes on Tab —
 * `DropdownMenu` handles Tab itself, per the APG menu-button pattern — so the
 * picker is not a keyboard trap. That is the property that matters and it is
 * the one pinned, in `new-wave.browser.test.tsx`, because none of it is
 * observable without a top layer.
 *
 * ## Why a HoverCard and not a Tooltip
 *
 * A tooltip is short non-interactive text and closes the moment the pointer
 * leaves the trigger, so a scrolling list inside one is unreachable — you
 * cannot move the mouse into it. `HoverCard` keeps itself open while the
 * pointer or focus is inside its content.
 *
 * ## The content
 *
 * Nothing here is authored copy: `key` and `goal` come from the `task` blocks
 * the created wave's report is seeded with, so this cannot drift from what the
 * template actually does the way a hand-written description would (#1209
 * declined to add one for exactly that reason).
 */
function TemplateChoice({ label, tasks, isSelected, onSelect }: Readonly<{
  label: string;
  tasks?: WaveTemplate['tasks'];
  isSelected: boolean;
  onSelect: () => void;
}>) {
  const item = (
    <DropdownMenuItem
      label={label}
      onClick={onSelect}
      /* `DropdownMenuItem` is a `role="menuitem"` with no `aria-checked` to
         set — astryx exposes no `menuitemradio`. The check icon alone would
         say nothing to a screen reader, so the state is also spelled out in
         the item's accessible name. */
      endContent={isSelected ? (
        <>
          <Icon icon="check" size="sm" color="accent" />
          <VisuallyHidden>Selected</VisuallyHidden>
        </>
      ) : undefined}
    />
  );
  if (tasks === undefined || tasks.length === 0) return item;
  return (
    <HoverCard
      placement="end"
      focusTrigger="always"
      content={(
        // Scrolling and a ceiling are ours: `HoverCard` has no max-height, and
        // its `className`/`xstyle` props never reach the rendered layer, so the
        // only place to bound the height is the content we pass in.
        <span className={styles.taskScroll}>
          <List listStyle="decimal" density="compact">
            {tasks.map((task) => (
              <ListItem key={task.key} label={task.key} description={task.goal} />
            ))}
          </List>
        </span>
      )}
    >
      {item}
    </HoverCard>
  );
}
