// The new-track page: one thing to say, and two optional chips under it. Presentational
// plus local form state; the caller owns `POST /api/tracks`, `submitting`, `error` and
// the template list, and puts the sentence on the create as `first_message`, verbatim.

import { useEffect, useMemo, useRef, useId, useCallback, type ReactNode } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { ChatComposer, ChatComposerInput, type ChatComposerTrigger } from '@astryxdesign/core/Chat';
import { Icon } from '@astryxdesign/core/Icon';
import { VStack } from '@astryxdesign/core/VStack';

import { isBlankForKernel, type TrackRecipe, type TrackTemplate } from '../../../../../core/domain/track.ts';
import type { ListDirectory } from '../../../ui/directory-browser/public.tsx';
import { DirectoryBrowser } from '../../../ui/directory-browser/public.tsx';
import { Dialog } from '../../../ui/dialog/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { triggerMenuKeyRoute } from '../../../ui/trigger-menu-keys/public.ts';
import {
  NO_STARTING_POINT, type StartingPoint,
} from '../default-pills/public.tsx';
import { compileTemplateInputs } from '../../../../../core/domain/template-input.ts';
import { TemplateInputs } from './template-inputs.tsx';
import type { LoadTemplate, TemplateDetail } from '../../../../../core/domain/template.ts';
import { TemplatePreview } from './template-preview.tsx';
import { NeigeMotion } from '../../../ui/brand/motion.tsx';
import styles from './new-track.module.css';
import { ComposerPreferences } from './composer-preferences.tsx';

/**
 * The starting point as a union: `template_id` and `recipe_id` are mutually
 * exclusive on the wire (the kernel answers both with a 400), so a draft carrying
 * both does not compile. Every arm names every key, so callers read without narrowing.
 */
type StartingPointFields =
  /** No starting point: neither id goes on the wire. */
  | Readonly<{ template_id?: undefined; template_input?: undefined; recipe_id?: undefined }>
  /** A built-in template, and the input it declared, if any. */
  | Readonly<{
    template_id: string;
    template_input?: Readonly<Record<string, unknown>>;
    recipe_id?: undefined;
  }>
  /** A user recipe; `template_input` is only accepted alongside `template_id`. */
  | Readonly<{ recipe_id: string; template_id?: undefined; template_input?: undefined }>;

export type NewTrackDraft = Readonly<{
  /**
   * What the user typed: the track's intent, not its title. Verbatim, whitespace
   * included, and never blank by the kernel's rule (`isBlankForKernel`).
   */
  message: string;
  /** Absolute path, or the key is absent: an empty string would take the attached branch with a path that cannot work. */
  cwd?: string;
}> & StartingPointFields;

export type NewTrackFormProps = Readonly<{
  /** App-composed model controls beside the send button. */
  modelControls?: ReactNode;
  /** App-composed, read-only documentation included by the selected template. */
  templatePluginGuides?: (templateId: string) => ReactNode;
  /** The `@` menu over this Area's reports, composed by the app: the sentence is the new track's Planner's first message. Kept stable by the caller. */
  mentionTrigger?: ChatComposerTrigger;
  submitting: boolean;
  error: string | null;
  /** In-memory route draft, including unfinished template input. */
  initialDraft?: NewTrackFormState;
  onDraftChange?: (draft: NewTrackFormState) => void;
  /** Keep the text available when its parent Area cannot accept a write. */
  submitBlocked?: boolean;
  /** An uncertain creation retries the original request instead of new edits. */
  locked?: boolean;
  /** One caller-owned recovery beside the create error; the form only renders the action and preserves its fields. */
  errorAction?: Readonly<{
    label: string;
    isApplicable?: (draft: NewTrackDraft) => boolean;
    onClick: (draft: NewTrackDraft) => void;
  }>;
  /** An empty roster is usable when the Area has no saved template; an unresolved Area preference blocks Create until the roster resolves or the reader picks No template. */
  templates: readonly TrackTemplate[];
  /** Read the selected author-owned template; the app supplies transport and authorization. */
  loadTemplate: LoadTemplate;
  /** Distinguishes an empty canonical roster from a read still in flight. */
  templatesLoaded: boolean;
  /** Set when the template read failed: a roster notice, not the create-error channel. */
  templatesError?: string | null;
  /** Snapshot of the Area preferences when this route opened. */
  initialTemplateId: string | null;
  initialCwd: string | null;
  /** The reader's own recipes; empty is the ordinary day-one state. Defaulted so a caller with no recipe read gets the built-ins-only picker. */
  recipes?: readonly TrackRecipe[];
  /** Open the manage-recipes screen; injected because `features/**` may not import `app/**`. */
  onManageRecipes: () => void;
  /** The folder picker's read port, created at the composition layer. Required: a call site that forgot it would render a picker that silently lists nothing. */
  listDirectory: ListDirectory;
  onSubmit: (draft: NewTrackDraft) => void;
}>;

/** The composer field's accessible name; hidden, not absent, since an unnamed textbox is unusable by screen reader. */
const TASK_LABEL = 'What this track should do';

const TASK_PLACEHOLDER = 'What should this track do?';

/** The way back to the managed default, which exists nowhere else. */
const FOLDER_CLEAR_LABEL = 'Use a Neige workspace instead';

export type NewTrackFormState = Readonly<{
  message: string;
  selected: StartingPoint;
  templateValues: Readonly<Record<string, Readonly<Record<string, string>>>>;
  cwd: string;
}>;

/** A template takes input iff a running trusted plugin is bound to it, i.e. the read returned an `input_schema`; with the plugin stopped the kernel would reject the fields. */
function needsInput(template: TrackTemplate | undefined): boolean {
  return template?.input_schema != null;
}

export function NewTrackForm({
  modelControls, templatePluginGuides, submitting, error, templates, templatesLoaded, templatesError = null,
  initialTemplateId, initialCwd, loadTemplate, recipes = [], onManageRecipes, listDirectory, onSubmit,
  errorAction, initialDraft, onDraftChange, submitBlocked = false, locked = false, mentionTrigger,
}: NewTrackFormProps) {
  const fieldId = useId();
  // Creation preferences are a route-opening snapshot: Area events may update this
  // prop while the route stays mounted, and a live value would silently clear an
  // unresolved opening default.
  const openingTemplateId = useRef(initialTemplateId).current;
  const [message, setMessage] = useState(initialDraft?.message ?? '');
  const [selected, setSelected] = useState<StartingPoint>(initialDraft?.selected ?? (openingTemplateId === null
    ? NO_STARTING_POINT
    : { kind: 'template', id: openingTemplateId }));
  const [templateValues, setTemplateValues] = useState(initialDraft?.templateValues ?? {});
  const [templateDetails, setTemplateDetails] = useState<Readonly<{ source: LoadTemplate; detail: TemplateDetail }> | null>(null);
  const receiveTemplateDetails = useCallback((detail: TemplateDetail | null) => {
    setTemplateDetails(detail === null ? null : { source: loadTemplate, detail });
  }, [loadTemplate]);
  const selectStartingPoint = (next: StartingPoint) => {
    if (next.kind === 'none' && selected.kind === 'none') return;
    if (next.kind !== 'none' && selected.kind !== 'none' && next.kind === selected.kind && next.id === selected.id) return;
    setTemplateDetails(null);
    setSelected(next);
  };
  const [cwd, setCwd] = useState(initialDraft?.cwd ?? initialCwd ?? '');
  const [browsing, setBrowsing] = useState(false);
  const composerHostRef = useRef<HTMLDivElement | null>(null);
  /* One array per trigger: `useTriggerMenu` drops an open menu when the list changes identity. */
  const triggers = useMemo(() => mentionTrigger === undefined ? undefined : [mentionTrigger], [mentionTrigger]);
  const folderId = `${fieldId}-folder`;
  const triggerId = `${fieldId}-start-from-trigger`;

  useEffect(() => {
    onDraftChange?.({ message, selected, templateValues, cwd });
  }, [message, selected, templateValues, cwd, onDraftChange]);

  /* The caret starts in the field. Found by query, not ref: `ChatComposerInput`
   * forwards its DOM `ref` to the wrapper, which is not focusable. Mount-only, so
   * a later render does not yank the caret from an opened chip. */
  useEffect(() => {
    const field = composerHostRef.current?.querySelector<HTMLElement>('[contenteditable="true"]');
    field?.focus();
  }, []);

  /* A starting point that vanished between renders falls back to no template, which
   * always submits; a persisted Area default is the exception and blocks Create
   * instead. Each lookup is confined to its own id space by the tag. */
  const chosen = selected.kind === 'template'
    ? templates.find((template) => template.id === selected.id)
    : undefined;
  const chosenRecipe = selected.kind === 'recipe'
    ? recipes.find((recipe) => recipe.id === selected.id)
    : undefined;
  const unresolvedAreaDefault = openingTemplateId !== null
    && selected.kind === 'template'
    && selected.id === openingTemplateId
    && chosen === undefined;
  const effectiveSelection: StartingPoint = selected.kind === 'none'
    || (selected.kind === 'template' && chosen !== undefined)
    || (selected.kind === 'recipe' && chosenRecipe !== undefined)
    || unresolvedAreaDefault
    ? selected
    : NO_STARTING_POINT;
  const preview = chosen ?? chosenRecipe;
  const wantsInput = needsInput(chosen);
  const currentDetails = templateDetails?.source === loadTemplate && templateDetails.detail.id === chosen?.id ? templateDetails.detail : null;
  const fieldValues = chosen === undefined ? {} : templateValues[chosen.id] ?? {};
  const compiledInputs = wantsInput && currentDetails !== null
    ? compileTemplateInputs(currentDetails.body, chosen?.input_schema, fieldValues) : null;
  const templatePending = unresolvedAreaDefault && !templatesLoaded && templatesError === null;
  const unsupportedInput = compiledInputs?.status === 'unsupported';
  const inputBlocker = unresolvedAreaDefault || (wantsInput && compiledInputs?.status !== 'ready');
  /* Blank by the kernel's rule, not JS `trim()`: the two disagree on a code point. */
  const valid = !isBlankForKernel(message) && (locked || !inputBlocker);
  /* One status slot; `templatesError` means the list is empty, so it never coexists
   * with an unsupported bound template. */
  const status = templatePending
    ? { type: 'warning' as const, message: 'Loading the Area’s default template…' }
    : unresolvedAreaDefault
      ? {
        type: 'error' as const,
        message: templatesError === null
          ? 'The Area’s default template is not available in this build. Choose another starting point.'
          : `${templatesError} Choose “No template” to continue without the saved default.`,
      }
      : unsupportedInput
    ? { type: 'error' as const, message: 'This template needs input this version cannot collect yet.' }
    : templatesError !== null
      ? { type: 'warning' as const, message: `${templatesError} You can still create a track without one.` }
      : undefined;

  function draftFor(text: string, forAction = false): NewTrackDraft | null {
    /* Blank refuses the submit; it does not rewrite it. `text` goes to the caller
           exactly as typed, and the kernel forwards it untrimmed. */
    if (isBlankForKernel(text) || (!locked && inputBlocker) || submitting || (submitBlocked && !forAction)) return null;
    /* Spread, not `cwd: cwd || undefined`: the caller keys managed-vs-attached on
           whether the key is there. A path travels byte-for-byte; leading/trailing
           spaces are legal POSIX path characters. */
    const base = { message: text, ...(cwd === '' ? {} : { cwd }) };
    if (effectiveSelection.kind === 'none') return base;
    /* A recipe carries `recipe_id` and stops here; the union has one arm at a time. */
    if (effectiveSelection.kind === 'recipe') {
      return { ...base, recipe_id: effectiveSelection.id };
    }
    return {
      ...base, template_id: effectiveSelection.id,
      ...(wantsInput && compiledInputs !== null && compiledInputs.status !== 'unsupported'
        ? { template_input: compiledInputs.input } : {}),
    };
  }

  function submit(text: string): void {
    const draft = draftFor(text);
    if (draft !== null) onSubmit(draft);
  }

  const actionDraft = draftFor(message, true);
  const shownErrorAction = errorAction !== undefined
    && actionDraft !== null
    && (errorAction.isApplicable?.(actionDraft) ?? true)
    ? errorAction
    : undefined;

  return (
    <div className={styles.page}>
      <VStack gap={2} className={styles.form}>


        <div className={styles.masthead}>
          <h1 className={styles.greeting}>
            <span className={styles.mark} aria-hidden="true"><NeigeMotion kind="creation" /></span>
            What would you like to work on?
          </h1>
        </div>

        <div
          ref={composerHostRef}
          className={styles.composer}
          data-nc-new-track-message
          /* Enter is ours: astryx's `ChatComposer.handleSubmit` clears the controlled value unconditionally after calling us, so a refused
             submit would lose the sentence. Only when the field itself is the target; Enter while composing is accepting an IME candidate, not sending. */
          onKeyDownCapture={(event) => {
            if (event.key !== 'Enter' && event.key !== 'Tab') return;
            const target = event.target as HTMLElement | null;
            const field = target?.closest?.('[contenteditable="true"]') ?? null;
            if (field === null) return;
            const route = triggerMenuKeyRoute(event.nativeEvent, field);
            if (route === 'composing' || route === 'swallow') {
              if (route === 'swallow') event.preventDefault();
              event.stopPropagation();
              return;
            }
            if (route === 'menu' || event.key !== 'Enter' || event.shiftKey) return;
            if (target !== field) {
              /* A control inside the editable: Enter belongs to it, so this neither submits
                               nor `preventDefault`s, but it must stop propagation or the editable
                               hands the event to astryx's own Enter handling, which submits. */
              event.stopPropagation();
              return;
            }
            event.stopPropagation();
            event.preventDefault();
            submit(message);
          }}
        >
          <ChatComposer
            density="spacious"
            value={message}
            onChange={setMessage}
            placeholder={TASK_PLACEHOLDER}
            isDisabled={submitting}
            onSubmit={submit}
            status={status}
            input={<div className={styles.editor}>
              {chosen !== undefined && templatePluginGuides?.(chosen.id)}
              <ChatComposerInput className={styles.input} label={TASK_LABEL} placeholder={TASK_PLACEHOLDER} isDisabled={submitting || locked}
                /* The `@` source delays its own requests; Astryx's delay would let its per-keystroke probe search fire. */
                {...(triggers === undefined ? {} : { triggers, debounceMs: 0 })} />
            </div>}
            footerActions={<ComposerPreferences browsing={browsing}
              startingPoint={{ templates, templatesLoaded, recipes, value: effectiveSelection,
                onChange: selectStartingPoint, onManageRecipes, placement: 'above', triggerId,
                isDisabled: submitting || locked }}
              folder={{ buttonId: folderId, value: cwd, clearLabel: FOLDER_CLEAR_LABEL,
                onBrowse: () => setBrowsing(true), onClear: () => setCwd(''),
                isDisabled: submitting || locked }}
            />}
            /* Model and Create stay together at the trailing edge. */
            sendActions={modelControls === undefined ? undefined : <span className={styles.modelControls}>{modelControls}</span>}
            sendButton={(
              <Button
                type="button"
                className={styles.send}
                variant="primary"
                isIconOnly
                icon={<Icon icon="arrowUp" size="sm" />}
                label={submitting ? 'Creating…' : 'Create track'}
                isDisabled={submitting || submitBlocked || !valid}
                onClick={() => submit(message)}
              />
            )}
          />
        </div>

        {error !== null && (
          <Banner
            status="error"
            title={error}
            endContent={shownErrorAction === undefined
              ? submitBlocked && message !== ''
                ? <Button label="Select draft" variant="ghost" onClick={() => {
                  /* The field itself, not its role: the `@` menu makes it a combobox. */
                  const field = composerHostRef.current?.querySelector<HTMLElement>('[contenteditable]');
                  if (field === null || field === undefined) return;
                  field.focus();
                  const range = document.createRange();
                  range.selectNodeContents(field);
                  const selection = window.getSelection();
                  selection?.removeAllRanges();
                  selection?.addRange(range);
                }} />
                : undefined
              : (
                <Button
                  label={shownErrorAction.label}
                  variant="ghost"
                  onClick={() => {
                    if (actionDraft !== null) shownErrorAction.onClick(actionDraft);
                  }}
                />
              )}
            data-nc-new-track-error
          />
        )}


        {preview !== undefined && (
          <TemplatePreview key={`${effectiveSelection.kind}:${preview.id}`}
            id={preview.id} title={preview.title}
            recipeBody={chosenRecipe?.body} loadTemplate={loadTemplate}
            onDetail={receiveTemplateDetails}>
            {compiledInputs !== null && compiledInputs.status !== 'unsupported' ? <TemplateInputs
              form={compiledInputs.form} values={fieldValues} errors={compiledInputs.errors}
              disabled={submitting || locked}
              onChange={(key, value) => {
                if (chosen === undefined) return;
                setTemplateValues((current) => ({ ...current, [chosen.id]: { ...current[chosen.id], [key]: value } }));
              }} /> : undefined}
          </TemplatePreview>
        )}
      </VStack>

      {/* The picker as a real modal: `Dialog` renders `null` while closed and owns the
                focus trap, Escape and click-outside. */}
      <Dialog
        open={browsing}
        onClose={() => setBrowsing(false)}
        title="Choose a directory"
        wide
      >
        <DirectoryBrowser
          listDirectory={listDirectory}
          initialPath={cwd === '' ? null : cwd}
          mode="directory"
          onCancel={() => setBrowsing(false)}
          onSelect={(path) => { setCwd(path); setBrowsing(false); }}
        />
      </Dialog>
    </div>
  );
}
