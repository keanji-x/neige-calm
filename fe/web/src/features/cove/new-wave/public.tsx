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

import { useId, type ReactNode, type RefObject } from 'react';

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
   */
  titleRef: RefObject<HTMLInputElement | null>;
  onCancel: () => void;
  onSubmit: (draft: NewWaveDraft) => void;
}>;

/** The one template whose inputs this form knows how to collect. */
const ISSUE_DEVELOPMENT = 'issue-development';

/**
 * Selection sentinel for Blank. `''` and not `null` so the value can be the
 * radio group's `value` without a cast; it is never sent — `buildDraft` omits
 * `workflow_id` for it.
 */
const BLANK = '';

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
  const titleId = `${fieldId}-title`;
  const groupId = `${fieldId}-start-from`;

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
  const inputBlocker = unsupportedInput
    ? 'This template needs input this version cannot collect yet.'
    : issueDev && parsedIssue === null
      ? issueUrl.trim() === ''
        ? 'Paste the GitHub issue this wave works on.'
        : 'Not a GitHub issue URL — expected https://github.com/owner/repo/issues/123.'
      : null;
  const valid = title.trim() !== '' && inputBlocker === null;

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
        <p className={styles.error} role="alert" data-nc-new-wave-error>{error}</p>
      )}

      <div className={styles.field}>
        <label className={styles.label} htmlFor={titleId}>Task</label>
        {/* Single-line, not a textarea: this value is the wave's `title`, and
            every other place that shows it — sidebar, wave list, page header —
            renders it as one truncated line, and the wave page edits it through
            the single-line `EditableTitle`. A three-row box was this one entry
            point promising a shape the rest of the app cannot keep.

            No `--font-mono` either: `fe-design.md:869` allows mono in a field
            only when it holds a prompt or a path, and a wave title is neither.
            It inherits the form's sans. */}
        <input
          ref={titleRef}
          id={titleId}
          className={styles.input}
          type="text"
          value={title}
          data-nc-new-wave-title
          onChange={(event) => setTitle(event.target.value)}
        />
      </div>

      <div className={styles.field}>
        <span className={styles.label} id={groupId}>Start from</span>
        {/* Native radios inside an explicit group: this picks one value out of
            a set, which is what `radiogroup` means, and the browser's own
            arrow-key and grouping behaviour comes free with `name`. A div with
            an onClick would have been a control the keyboard cannot reach. */}
        <div className={styles.list} role="radiogroup" aria-labelledby={groupId}>
          <TemplateRow
            fieldId={fieldId}
            value={BLANK}
            title="Blank"
            checked={effectiveSelection === BLANK}
            onSelect={setSelected}
          />
          {templates.map((template) => {
            const rowSelected = effectiveSelection === template.id;
            return (
              <TemplateRow
                key={template.id}
                fieldId={fieldId}
                value={template.id}
                title={template.title}
                checked={rowSelected}
                expandedId={needsInput(template) && rowSelected ? `${fieldId}-${template.id}-input` : undefined}
                onSelect={setSelected}
              >
                {rowSelected && wantsInput && (
                  <div
                    className={styles.expand}
                    id={`${fieldId}-${template.id}-input`}
                    role="group"
                    aria-labelledby={`${fieldId}-${template.id}-label`}
                  >
                    {unsupportedInput ? (
                      <p className={styles.hint} role="status">{inputBlocker}</p>
                    ) : (
                      <>
                        <div className={styles.field}>
                          <label className={styles.label} htmlFor={`${fieldId}-issue-url`}>Issue URL</label>
                          <input
                            id={`${fieldId}-issue-url`}
                            className={styles.input}
                            type="text"
                            value={issueUrl}
                            placeholder="https://github.com/owner/repo/issues/123"
                            aria-invalid={issueUrl.trim() !== '' && parsedIssue === null}
                            aria-describedby={`${fieldId}-issue-url-hint`}
                            onChange={(event) => setIssueUrl(event.target.value)}
                          />
                          {/* `status`, not `alert`: an unfinished field is not
                              an error condition, and the submit button already
                              carries the consequence. */}
                          <p className={styles.hint} id={`${fieldId}-issue-url-hint`} role="status">
                            {inputBlocker ?? `Issue #${parsedIssue?.issue_number} in ${parsedIssue?.repo}.`}
                          </p>
                        </div>
                        <label className={styles.check}>
                          <input
                            type="checkbox"
                            checked={autoMerge}
                            onChange={(event) => setAutoMerge(event.target.checked)}
                          />
                          Merge automatically once the gates converge
                        </label>
                        <p className={styles.hint}>Off: the wave waits for you to approve the merge.</p>
                      </>
                    )}
                  </div>
                )}
              </TemplateRow>
            );
          })}
        </div>
        {templatesError !== null && (
          /* Not `alert`: nothing the user did failed, and nothing they wanted
             is blocked — Blank is right there. */
          <p className={styles.hint} role="status" data-nc-new-wave-templates-error>
            {templatesError} You can still create a blank wave.
          </p>
        )}
      </div>

      <div className={styles.actions}>
        <button type="button" className={styles.cancel} onClick={onCancel}>Cancel</button>
        <button type="submit" className={styles.submit} disabled={submitting || !valid}>
          {submitting ? 'Creating…' : 'Create wave'}
        </button>
      </div>
    </form>
  );
}

type TemplateRowProps = Readonly<{
  fieldId: string;
  value: string;
  title: string;
  checked: boolean;
  /** Set when this row owns an expanded input panel, for `aria-controls`. */
  expandedId?: string;
  onSelect: (value: string) => void;
  children?: ReactNode;
}>;

function TemplateRow({ fieldId, value, title, checked, expandedId, onSelect, children }: TemplateRowProps) {
  const labelId = `${fieldId}-${value === BLANK ? 'blank' : value}-label`;
  return (
    <div className={checked ? `${styles.row} ${styles.rowOn}` : styles.row}>
      <label className={styles.rowLabel} id={labelId}>
        <input
          type="radio"
          name={`${fieldId}-template`}
          value={value}
          checked={checked}
          // `aria-controls` only: `aria-expanded` is not supported on role
          // `radio`, and the panel is not a disclosure the reader toggles —
          // it appears because this alternative was chosen.
          aria-controls={expandedId}
          onChange={() => onSelect(value)}
        />
        {title}
      </label>
      {children}
    </div>
  );
}
