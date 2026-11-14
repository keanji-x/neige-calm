import type { DialogOrigin } from '../../../ui/dialog/public.tsx';
// The new-track page: one thing to say, with compact preferences in its footer. Presentational
// plus local form state; the caller owns `POST /api/tracks`, `submitting`, `error` and
// puts the sentence on the create as `first_message`, verbatim.

import { useEffect, useMemo, useRef, useId, type ReactNode } from 'react';
import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { ChatComposer, ChatComposerInput, type ChatComposerTrigger } from '@astryxdesign/core/Chat';
import { Icon } from '@astryxdesign/core/Icon';
import { VStack } from '@astryxdesign/core/VStack';

import { isBlankForKernel } from '../../../../../core/domain/track.ts';
import type { ListDirectory } from '../../../ui/directory-browser/public.tsx';
import { DirectoryBrowser } from '../../../ui/directory-browser/public.tsx';
import { Dialog } from '../../../ui/dialog/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { triggerMenuKeyRoute } from '../../../ui/trigger-menu-keys/public.ts';
import { NeigeMotion } from '../../../ui/brand/motion.tsx';
import styles from './new-track.module.css';
import { ComposerPreferences } from './composer-preferences.tsx';

export type NewTrackDraft = Readonly<{
  /**
   * What the user typed: the track's intent, not its title. Verbatim, whitespace
   * included, and never blank by the kernel's rule (`isBlankForKernel`).
   */
  message: string;
  /** Absolute path, or the key is absent: an empty string would take the attached branch with a path that cannot work. */
  cwd?: string;
  isolation?: 'none';
}>;

export type NewTrackFormProps = Readonly<{
  /** App-composed model controls beside the send button. */
  modelControls?: ReactNode;
  /** The `@` menu over this Area's reports, composed by the app: the sentence is the new track's Planner's first message. Kept stable by the caller. */
  mentionTrigger?: ChatComposerTrigger;
  submitting: boolean;
  error: string | null;
  /** Caller-owned read feedback; the draft remains editable while a read is pending. */
  readNotice?: ReactNode;
  /** In-memory route draft, including the unfinished message. */
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
  initialCwd: string | null;
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
  cwd: string;
}>;

export function NewTrackForm({
  modelControls, submitting, error, readNotice, initialCwd, listDirectory, onSubmit,
  errorAction, initialDraft, onDraftChange, submitBlocked = false, locked = false, mentionTrigger,
}: NewTrackFormProps) {
  const fieldId = useId();
  const [message, setMessage] = useState(initialDraft?.message ?? '');
  const [cwd, setCwd] = useState(initialDraft?.cwd ?? initialCwd ?? '');
  const isolation = 'none' as const;
  const [browsing, setBrowsing] = useState<DialogOrigin | null>(null);
  const composerHostRef = useRef<HTMLDivElement | null>(null);
  /* One array per trigger: `useTriggerMenu` drops an open menu when the list changes identity. */
  const triggers = useMemo(() => mentionTrigger === undefined ? undefined : [mentionTrigger], [mentionTrigger]);
  const folderId = `${fieldId}-folder`;

  useEffect(() => {
    onDraftChange?.({ message, cwd });
  }, [message, cwd, onDraftChange]);

  /* The caret starts in the field. Found by query, not ref: `ChatComposerInput`
   * forwards its DOM `ref` to the wrapper, which is not focusable. Mount-only, so
   * a later render does not yank the caret from an opened chip. */
  useEffect(() => {
    const field = composerHostRef.current?.querySelector<HTMLElement>('[contenteditable="true"]');
    field?.focus();
  }, []);

  const valid = !isBlankForKernel(message);

  function draftFor(text: string, forAction = false): NewTrackDraft | null {
    /* Blank refuses the submit; it does not rewrite it. `text` goes to the caller
           exactly as typed, and the kernel forwards it untrimmed. */
    if (isBlankForKernel(text) || submitting || (submitBlocked && !forAction)) return null;
    /* Spread, not `cwd: cwd || undefined`: the caller keys managed-vs-attached on
           whether the key is there. A path travels byte-for-byte; leading/trailing
           spaces are legal POSIX path characters. */
    return { message: text, ...(cwd === '' ? {} : { cwd, isolation }) };
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

        <p className={styles.introduction}>
          Describe your request and optionally choose a Repo for your repository.
          An Area groups a project&apos;s work; a Track keeps one request, its conversation and report.
        </p>

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
            input={<div className={styles.editor}>
              <ChatComposerInput className={styles.input} label={TASK_LABEL} placeholder={TASK_PLACEHOLDER} isDisabled={submitting || locked}
                /* The `@` source delays its own requests; Astryx's delay would let its per-keystroke probe search fire. */
                {...(triggers === undefined ? {} : { triggers, debounceMs: 0 })} />
            </div>}
            footerActions={<ComposerPreferences browsing={browsing !== null}
              folder={{ buttonId: folderId, value: cwd, clearLabel: FOLDER_CLEAR_LABEL,
                onBrowse: (origin) => setBrowsing(origin), onClear: () => { setCwd(''); },
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

        {readNotice}
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

      </VStack>

      {/* The picker as a real modal: `Dialog` renders `null` while closed and owns the
                focus trap, Escape and click-outside. */}
      <Dialog purpose="form"
        origin={browsing ?? { kind: 'surface-center' }}
        open={browsing !== null}
        onClose={() => setBrowsing(null)}
        title="Choose a directory"
        wide
      >
        <DirectoryBrowser
          listDirectory={listDirectory}
          initialPath={cwd === '' ? null : cwd}
          mode="directory"
          onCancel={() => setBrowsing(null)}
          onSelect={(path) => { setCwd(path); setBrowsing(null); }}
        />
      </Dialog>
    </div>
  );
}
