// The new-wave form: a task, and what the wave starts from.
//
// Presentational + local form state — it never calls an API. The caller owns
// POST /api/waves, `submitting`, `error`, and the template list itself.
//
// `cove_id` is not a form field. The dialog opens from a cove page `+` (or the
// rail's per-cove `+`); the caller already knows which cove and sends it on
// the request. Binding a folder is out of scope: the kernel defaults omitted
// `cwd` to `$HOME` and does not claim it.
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
// ## Start from (#1209)
//
// `Blank` is a first-class option and the default, and it is **not** a row the
// server sent: it is the absence of a template, i.e. a create with no
// `workflow_id` — precisely today's behaviour. Everything about this list is
// arranged so that staying on Blank is free. In particular `templates` may be
// empty because the read failed or has not landed, and the dialog is fully
// usable in that state: this is the app's only wave-creation entry point, and
// a failed list read must not be able to close it.
//
// The vocabulary seam is deliberate and recorded in #1209: the read side says
// *template*, the write side says `workflow_id`. This form speaks the read
// side's word to the user and the write side's word on the wire.

import { useId, type RefObject } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { CheckboxInput } from '@astryxdesign/core/CheckboxInput';
import { Field } from '@astryxdesign/core/Field';
import { HoverCard } from '@astryxdesign/core/HoverCard';
import { List, ListItem } from '@astryxdesign/core/List';
import { RadioList, RadioListItem } from '@astryxdesign/core/RadioList';
import { TextInput } from '@astryxdesign/core/TextInput';

import { parseGitHubIssueUrl } from '../../../../../core/domain/issue-url.ts';
import type { WaveTemplate } from '../../../../../core/domain/wave.ts';
import { useState } from '../../../ui/state/public.ts';
import styles from './new-wave.module.css';

export type NewWaveDraft = Readonly<{
  title: string;
  /** Absent for Blank — never `null` or `''`, which the kernel 400s. */
  workflow_id?: string;
  workflow_input?: Readonly<Record<string, unknown>>;
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
 * `''` because `RadioList.value` is a string and Blank is the *absence* of a
 * template id, which no server row can ever collide with. astryx reads `''` as
 * "nothing selected" for one purpose only — a focus-entry correction for
 * groups with no selection — and that correction is a no-op here: it redirects
 * to the first enabled radio, which is Blank, the radio that is checked.
 */
const BLANK = '';

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
  submitting, error, templates, templatesError = null, titleRef, onCancel, onSubmit,
}: NewWaveFormProps) {
  const fieldId = useId();
  const [title, setTitle] = useState('');
  const [selected, setSelected] = useState<string>(BLANK);
  const [issueUrl, setIssueUrl] = useState('');
  const [autoMerge, setAutoMerge] = useState(false);
  const panelLabelId = `${fieldId}-panel-label`;
  /*
   * `Field` requires an `inputID` even as a group label, where it is unused:
   * a group label renders as a `<span>`, which has no `htmlFor`. The group's
   * own controls are astryx inputs that mint their own ids, so there is no
   * single control to point at — this satisfies the prop and names nothing.
   */
  const unusedPanelInputId = `${fieldId}-panel-input`;

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
   * One status slot on the group, and the two things that can fill it never
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
    if (effectiveSelection === BLANK) return { title: title.trim() };
    if (parsedIssue === null) return { title: title.trim(), workflow_id: effectiveSelection };
    // The kernel applies no schema defaults, so `merge_policy` always travels
    // explicitly. Unchecked is `hold-for-ratify`: the default direction is
    // "wait for a human", and flipping it would auto-merge by omission.
    const mergePolicy: MergePolicy = autoMerge ? 'auto-merge' : 'hold-for-ratify';
    return {
      title: title.trim(),
      workflow_id: effectiveSelection,
      workflow_input: { ...parsedIssue, merge_policy: mergePolicy },
    };
  }

  return (
    <form
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

      {/* `RadioList` and not `SelectableCard`: these are mutually exclusive
          alternatives, so the control has to be a radio group — real radios,
          one tab stop, arrow keys between them. `SelectableCard` puts a hidden
          *checkbox* behind each card, which announces "one of these is
          independently on or off". */}
      <RadioList
        label="Start from"
        value={effectiveSelection}
        onChange={setSelected}
        status={groupStatus}
      >
        <RadioListItem label="Blank" value={BLANK} />
        {templates.map((template) => (
          <RadioListItem
            key={template.id}
            label={template.title}
            value={template.id}
            endContent={<TemplateTasks template={template} />}
          />
        ))}
      </RadioList>

      {issueDev && (
        /* The panel sits after the group rather than inside the chosen row:
           `RadioListItem` takes no children, and a form panel spliced between
           two radios inside `role="radiogroup"` is not a shape the pattern
           allows. astryx's own group-label mechanism carries the association
           instead — a `<span>` label (never a `<label>`, which names exactly
           one control) that the group points at with `aria-labelledby`. */
        <Field
          label={chosen?.title ?? ''}
          labelID={panelLabelId}
          isGroupLabel
          inputID={unusedPanelInputId}
        >
          <div className={styles.panel} role="group" aria-labelledby={panelLabelId}>
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
        </Field>
      )}

      <div className={styles.actions}>
        <Button type="button" label="Cancel" variant="ghost" onClick={onCancel} />
        <Button
          type="submit"
          variant="primary"
          label={submitting ? 'Creating…' : 'Create wave'}
          isDisabled={submitting || !valid}
        />
      </div>
    </form>
  );
}

/**
 * "What will this template give me", answered with the template's own plan.
 *
 * The count is the trigger and is itself the headline fact; the card behind it
 * is the list. Nothing here is authored copy: `key` and `goal` come from the
 * `task` blocks the created wave's report is seeded with, so this cannot drift
 * from what the template actually does the way a hand-written description
 * would (#1209 declined to add one for exactly that reason).
 *
 * `HoverCard`, not `Tooltip`. A tooltip is short non-interactive text and
 * closes the moment the pointer leaves the trigger, so a scrolling list inside
 * one is unreachable — you cannot move the mouse into it. `HoverCard` keeps
 * itself open while the pointer or focus is inside its content, which is what
 * makes eight scrollable rows usable.
 *
 * Keyboard: passing a plain string as the child makes astryx render the
 * trigger as a focusable `<span tabIndex={0}>` with `aria-describedby` bound
 * to the card, and attach the focus listeners that open it — so the content is
 * reachable by Tab and dismissible with Escape, not hover-only.
 */
function TemplateTasks({ template }: Readonly<{ template: WaveTemplate }>) {
  const count = template.tasks.length;
  return (
    <HoverCard
      placement="above"
      content={(
        // Scrolling and a ceiling are ours: `HoverCard` has no max-height, and
        // its `className`/`xstyle` props never reach the rendered layer, so the
        // only place to bound the height is the content we pass in.
        <span className={styles.taskScroll}>
          <List listStyle="decimal" density="compact">
            {template.tasks.map((task) => (
              <ListItem key={task.key} label={task.key} description={task.goal} />
            ))}
          </List>
        </span>
      )}
    >
      {count === 1 ? '1 task' : `${count} tasks`}
    </HoverCard>
  );
}
