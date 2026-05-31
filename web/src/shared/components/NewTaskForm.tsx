// ---------------- NewTaskForm ----------------
//
// Issue #250 PR 3 — the single creation surface for "task = description
// + cwd + cove". Used by the cove page's `+ New wave` affordance today,
// the calendar's empty-cell click later (PR 6). Per the issue comment
// "all creation entrypoints must go through the same configuration
// card", this component is the only place that knows how to POST a
// well-formed `NewWave` body — every other entrypoint must reuse it
// (not re-implement the cwd/cove inference).
//
// Field semantics (decided across #255 + #250 PR 2, updated by #409):
//   * task description (optional) → posted as `wave.title`. The kernel
//     threads a non-empty title into the spec daemon as the initial
//     prompt; an empty title boots the spec daemon without an
//     auto-submitted prompt. We deliberately do not surface a separate
//     "prompt" field — title-as-prompt keeps the wave-row label and the
//     prompt in lock-step when a prompt exists.
//   * cwd (required) → absolute path the spec daemon spawns under.
//     The form refuses to submit a non-`/`-prefixed value; the server
//     would 400 anyway, but inline rejection is cheaper than a round
//     trip + read of an error toast.
//   * cove (required) → derived from cwd via `GET /api/coves/resolve`:
//       - hit  → field locked to the auto-matched cove; submit goes
//                straight through (no `attach_folder` opt-in needed,
//                the cwd is already under that cove's folder claim).
//       - miss → field user-editable, two paths:
//                  "existing": pick a cove from `useCovesQuery`, submit
//                    with `attach_folder: true` so the kernel adds
//                    `cwd` as a new folder under that cove inside the
//                    same tx as the wave-create. PR 3 implements this
//                    as a two-step client-side flow for the "create
//                    new cove + claim cwd" branch (see below) — the
//                    "existing cove + attach cwd" branch is a single
//                    POST with `attach_folder: true`, which the kernel
//                    handles atomically inside one tx.
//                  "new":      mint a fresh cove (`POST /api/coves`),
//                    then POST the wave with the new id + the same
//                    `attach_folder: true` flag. Two-step (cove +
//                    wave) because the server doesn't yet expose an
//                    atomic "create cove + first wave" endpoint; if
//                    the wave POST fails (e.g. validation), the cove
//                    is left in place and a retry reuses it. Followup
//                    todo to collapse this into one atomic endpoint
//                    if the leftover-cove cost ever bites.
//
// 409 / FolderConflict handling: NewWave's `attach_folder: true` path
// can land a structured 409 on this list of scenarios:
//   - cwd is descendant of a folder owned by a *different* cove (the
//     resolve would have warned us pre-submit, but a concurrent claim
//     can still race here);
//   - cwd is an ancestor of an existing narrower claim (widening, the
//     server refuses for resolution ambiguity).
// The form reads the `{cove_id, conflict_path, conflict_kind}` body
// and renders a one-line, user-readable diagnosis without leaking the
// raw enum into the UI.
//
// A11y: every input has a real <label> (htmlFor + id); the wrapping
// section is `role="form"` with a labelled heading so a Playwright
// `getByRole('form', { name: 'New task' })` lookup is unambiguous in
// dense pages (Cove page below + calendar later).

import { useCallback, useEffect, useId, useMemo, useRef } from 'react';
import type { RefObject } from 'react';
import { useState } from '../state';
import { useQueryClient } from '@tanstack/react-query';
import * as api from '../../api/calm';
import { CalmApiError } from '../../api/calm';
import {
  queryKeys,
  useCovesQuery,
  useCreateCoveMutation,
  useCreateWaveMutation,
} from '../../api/queries';
import { DARK_THEME_RGB, LIGHT_THEME_RGB } from '../../api/themeRgb';
import type { CoveResolveBody, FolderConflictBody, KernelWave } from '../../api/wire';
import { DirectoryBrowser } from './DirectoryPicker';
import { useModalView } from '../../ui/Dialog/Dialog';

/** Result handed back to the caller on successful POST `/api/waves`. */
export type NewTaskFormResult = KernelWave;

export interface NewTaskFormProps {
  /** Pre-selected cove. When the surrounding page already scopes itself
   *  to a cove (cove page) we pass it here so the dropdown defaults to
   *  it on first paint. The cwd-resolve auto-match still overrides this
   *  if it lands a different cove. */
  defaultCoveId?: string;
  /** Fired after the wave-create POST succeeds. Caller usually navigates
   *  to `/calm/wave/<id>`. */
  onCreated: (wave: NewTaskFormResult) => void | Promise<void>;
  /** Fired when the user dismisses the form (Esc, Cancel). Caller
   *  collapses the inline panel back to a CTA button. */
  onCancel: () => void;
  /** Optional ref forwarded to the title textarea. When provided, the
   *  caller (typically a host `<Dialog>`) uses this to claim initial
   *  focus on the title input — the form skips its own
   *  `queueMicrotask(focus)` mount effect to avoid racing against the
   *  Dialog's rAF "focus first focusable" pass, which otherwise lands
   *  focus on the Dialog's Close button. When omitted, the form falls
   *  back to focusing the title field itself on mount. */
  initialFocusRef?: RefObject<HTMLTextAreaElement | null>;
}

/** Debounce window for the cwd → resolve API call. 300ms balances
 *  "feels live" against "didn't fire a request after every keypress". */
const RESOLVE_DEBOUNCE_MS = 300;

/** Fallback palette for the "create new cove" branch — same set
 *  Sidebar's `NewCoveButton` draws from. Keep in lockstep; a real
 *  color picker is a future enhancement. */
const COVE_PALETTE = ['#5a9', '#c97', '#79c', '#b86', '#6a8', '#a6c'];

type CoveChoice =
  | { mode: 'auto'; resolve: CoveResolveBody }
  | { mode: 'existing'; coveId: string }
  | { mode: 'new'; name: string; color: string };

export function NewTaskForm({
  defaultCoveId,
  onCreated,
  onCancel,
  initialFocusRef,
}: NewTaskFormProps) {
  const titleId = useId();
  const cwdId = useId();
  const coveSelectId = useId();
  const newCoveNameId = useId();
  const headingId = useId();

  const [title, setTitle] = useState('');
  const [cwd, setCwd] = useState('');
  const [resolveState, setResolveState] = useState<
    | { kind: 'idle' }
    | { kind: 'resolving' }
    | { kind: 'hit'; resolve: CoveResolveBody }
    | { kind: 'miss' }
  >({ kind: 'idle' });
  // When the resolve misses, the user picks between "existing cove" and
  // "new cove". Default to "existing" if a `defaultCoveId` was passed
  // (caller already has one in mind); otherwise "new" — the user
  // typed a cwd nobody owns, "create a cove for this" is the obvious
  // next step.
  const covesQ = useCovesQuery();
  const coves = useMemo(() => covesQ.data ?? [], [covesQ.data]);

  // Deterministic palette seed: cycle through COVE_PALETTE by the
  // current cove count so the "Create new cove" branch picks a stable
  // color for the same UI state (no Math.random flake in tests, no
  // jitter between renders for the same user state).
  const seededPaletteColor = useCallback(
    () => pickPaletteColor(coves.length),
    [coves.length],
  );

  const [coveChoice, setCoveChoice] = useState<CoveChoice>(() =>
    defaultCoveId
      ? { mode: 'existing', coveId: defaultCoveId }
      : { mode: 'new', name: '', color: pickPaletteColor(0) },
  );
  const [submitting, setSubmitting] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  // Tracks whether the user has explicitly overridden an auto-match. A
  // hit lands → we set coveChoice to `{ mode: 'auto', ... }` AND clear
  // this flag so future resolves can also auto-match. Once the user
  // clicks "Use a different cove", we set this flag; subsequent
  // resolves still update resolveState (so the banner can still
  // describe what the cwd matches), but they no longer overwrite the
  // user's manual coveChoice.
  const userOverrodeAutoMatchRef = useRef(false);

  const createWave = useCreateWaveMutation();
  const createCove = useCreateCoveMutation();
  const qc = useQueryClient();
  // Browse… → always pushes a DirectoryBrowser view into the surrounding
  // Dialog's body via `useModalView()`. NewTaskForm is hosted exclusively
  // inside a Dialog today (NewWaveCTA in Cove.tsx wraps it), so the
  // modal-view context is always present in production. The dev-time
  // `console.warn` below catches accidental Dialog-less renderings during
  // refactors instead of silently breaking the Browse affordance.
  const modalView = useModalView();

  const localTitleRef = useRef<HTMLTextAreaElement | null>(null);
  // When a caller forwards `initialFocusRef`, use it as the title
  // textarea's ref — the host Dialog will own initial focus. Otherwise
  // fall back to our own ref + the mount-time focus effect below.
  const titleRef = initialFocusRef ?? localTitleRef;
  const cwdRef = useRef<HTMLInputElement | null>(null);
  // Focus the title field on mount — opening the form should land the
  // caret in the first meaningful input without an extra click. Skipped
  // when the caller forwarded `initialFocusRef`: the Dialog's own
  // rAF-deferred focus pass would race against this microtask and
  // sometimes win (landing on the Dialog Close button), so the contract
  // is "Dialog focuses for us, we don't double-focus".
  useEffect(() => {
    if (initialFocusRef) return;
    queueMicrotask(() => localTitleRef.current?.focus());
  }, [initialFocusRef]);

  // Latest cwd at commit-time. The resolve effect captures `cwd` via
  // closure, but the in-flight `api.resolveCovePath` Promise may resolve
  // after the user has typed more characters — without this guard a
  // stale resolve would overwrite a fresher one (`Math.random`-ish
  // ordering of `fetch` completions across two debounce windows). The
  // ref is the single source of truth that's read at commit-time.
  const latestResolveCwdRef = useRef<string>('');

  // Debounced cwd → resolve. We do NOT clear the existing auto-match
  // on every keystroke — that would flicker the "auto-matched to X"
  // banner mid-typing. Instead, the resolveState transitions only when
  // the debounce window fires and we have a fresh answer.
  useEffect(() => {
    const trimmed = cwd.trim();
    if (!isAbsolutePath(trimmed)) {
      // Non-absolute input keeps the resolve in `idle` (no banner) —
      // the inline cwd-error already explains the shape requirement.
      // We also clear the latest-cwd ref so any in-flight stale resolve
      // can't sneak in a commit against an emptied input.
      latestResolveCwdRef.current = '';
      setResolveState({ kind: 'idle' });
      return;
    }
    // Mark this cwd as the latest one we want a resolve for; the
    // commit-time check below compares against this ref to drop any
    // stale in-flight resolve.
    latestResolveCwdRef.current = trimmed;
    setResolveState({ kind: 'resolving' });
    const timer = setTimeout(() => {
      void (async () => {
        try {
          const hit = await api.resolveCovePath(trimmed);
          // Race guard: drop the result if the user has typed past this
          // cwd since the request fired. Without this check, two
          // overlapping resolves can land out-of-order and the stale
          // one wins.
          if (latestResolveCwdRef.current !== trimmed) return;
          if (hit) {
            setResolveState({ kind: 'hit', resolve: hit });
            // Once a hit lands, the cove choice is forced — unless the
            // user has explicitly overridden a previous auto-match, in
            // which case the banner still updates (so the user sees
            // what the cwd matches) but the manual coveChoice stands.
            if (!userOverrodeAutoMatchRef.current) {
              setCoveChoice({ mode: 'auto', resolve: hit });
            }
          } else {
            setResolveState({ kind: 'miss' });
            // On miss, fall back to the default coveChoice that was
            // seeded at mount — but only if we're currently in `auto`
            // (a previous hit). Otherwise the user's pick stands.
            setCoveChoice((cur) =>
              cur.mode === 'auto'
                ? defaultCoveId
                  ? { mode: 'existing', coveId: defaultCoveId }
                  : { mode: 'new', name: '', color: seededPaletteColor() }
                : cur,
            );
          }
        } catch (e) {
          // Same race-guard rule for the error path: if the user typed
          // past this cwd, drop the error too — the newer resolve will
          // own the UI state.
          if (latestResolveCwdRef.current !== trimmed) return;
          // Resolve failure (network etc.) — surface as miss so the
          // user can still pick / create a cove. The submit path will
          // re-validate via the server.
          setResolveState({ kind: 'miss' });
          // Keep the inline error visible if the resolve failed mid-
          // typing; the user can still proceed via manual cove pick.
          if (e instanceof CalmApiError && e.status !== 400) {
            setErrorMsg(`Path lookup failed: ${e.message}`);
          }
        }
      })();
    }, RESOLVE_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [cwd, defaultCoveId, seededPaletteColor]);

  const cwdError = cwd.length > 0 && !isAbsolutePath(cwd.trim())
    ? 'Path must be absolute (start with `/`).'
    : null;

  const canSubmit = canSubmitForm({
    cwd,
    cwdError,
    coveChoice,
    submitting,
  });

  const handleSubmit = useCallback(
    async (e?: React.FormEvent | React.KeyboardEvent) => {
      e?.preventDefault();
      if (!canSubmit) return;
      setSubmitting(true);
      setErrorMsg(null);
      try {
        const finalCwd = cwd.trim();
        // Resolve the cove_id + attach_folder flag from the form state:
        //   * auto → cwd already covered; attach=false
        //   * existing → user-picked; attach=true so the cwd lands as a
        //     folder under that cove inside the wave-create tx.
        //   * new → mint the cove first, then submit the wave under it
        //     with attach=true.
        let coveId: string;
        let attachFolder: boolean;
        if (coveChoice.mode === 'auto') {
          coveId = coveChoice.resolve.cove_id;
          attachFolder = false;
        } else if (coveChoice.mode === 'existing') {
          coveId = coveChoice.coveId;
          attachFolder = true;
        } else {
          // Two-step: cove first, then wave. If the wave POST fails
          // the cove is left in place — see file header for rationale.
          // TODO(#250): atomic create-cove-and-wave endpoint to collapse
          // this two-step and remove the leftover-cove risk on partial
          // failure (current fallback: a retry reuses the orphan cove).
          const cove = await createCove.mutateAsync({
            name: coveChoice.name.trim(),
            color: coveChoice.color,
          });
          coveId = cove.id;
          attachFolder = true;
          // The new cove is already in `useCovesQuery` cache via the
          // mutation's onSuccess invalidate. No extra work here.
        }

        const wave = await createWave.mutateAsync({
          cove_id: coveId,
          title: title.trim(),
          cwd: finalCwd,
          attach_folder: attachFolder,
          theme: readHostThemeRgb(),
        });
        // Belt-and-suspenders cache invalidate — useCreateWaveMutation
        // already kicks ['waves', cove_id], but a brand-new cove also
        // benefits from a coves-list refresh in case the WS event
        // didn't land yet.
        void qc.invalidateQueries({ queryKey: queryKeys.coves() });
        await onCreated(wave);
      } catch (e) {
        const formatted = formatSubmitError(e, coves);
        setErrorMsg(formatted);
      } finally {
        setSubmitting(false);
      }
    },
    [canSubmit, coveChoice, coves, createCove, createWave, cwd, onCreated, qc, title],
  );

  // Escape from anywhere inside the form cancels. Submit-on-Enter is
  // wired per-field rather than at the form level because the title
  // textarea must allow newlines.
  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Escape') {
      e.preventDefault();
      onCancel();
    }
  };

  // Browse… handler. Always pushes the DirectoryBrowser into the
  // surrounding Dialog's body via `useModalView()` — same affordance the
  // codex card uses, no nested popover. The initialPath is the current
  // cwd if it looks absolute (we let the server fall through to $HOME
  // otherwise via `null`). If `useModalView()` returns null we're
  // rendered outside a Dialog, which only happens by mistake; warn once
  // in dev and no-op so the visible Browse button doesn't appear to do
  // anything (better than a confusing crash on click).
  const startBrowse = useCallback(() => {
    const seed = isAbsolutePath(cwd.trim()) ? cwd.trim() : null;
    if (!modalView) {
      if (import.meta.env?.DEV) {
        console.warn(
          '[NewTaskForm] Browse… clicked outside a <Dialog> — no modal-view context. Wrap NewTaskForm in <Dialog> to enable the directory picker.',
        );
      }
      return;
    }
    const commit = (path: string) => {
      setCwd(path);
      modalView.popView();
    };
    const cancel = () => modalView.popView();
    modalView.pushView({
      title: 'Choose a directory',
      onEscape: cancel,
      body: (
        <DirectoryBrowser
          initialPath={seed}
          onCancel={cancel}
          onSelect={commit}
        />
      ),
    });
  }, [cwd, modalView]);

  return (
    <section
      role="form"
      aria-labelledby={headingId}
      className="new-task-form"
    >
      <h2 id={headingId} className="new-task-form-heading">
        New task
      </h2>
      {/* Form-level Escape listener cancels the inline panel. The
          rule warns because <form> is not in a11y's "interactive"
          allowlist, but Esc-to-cancel on the *form's* focused
          descendants is the natural keyboard contract for a config
          card. */}
      {/* eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions */}
      <form
        onSubmit={(e) => {
          void handleSubmit(e);
        }}
        onKeyDown={handleKeyDown}
      >
        {/* Task description ↔ wave.title. Textarea so the user can
            paste a multi-line ask without us truncating. Enter is
            *not* submit here — newlines in the description are
            valid; submit is the explicit "Create task" button.
            Empty is also valid: the spec daemon boots with no
            auto-submitted prompt. */}
        <label htmlFor={titleId} className="new-task-form-label">
          Task description
        </label>
        <textarea
          id={titleId}
          ref={titleRef}
          className="new-task-form-input"
          rows={3}
          value={title}
          onChange={(e) => setTitle(e.target.value)}
          placeholder="What should the agent do?"
        />

        {/* cwd — absolute path. Submit-on-Enter lives here because the
            common path is "type the cwd, press Enter"; cwd is the
            required field that gates submit. The inline error sits
            directly under the input so it pairs visually with the
            field that triggered it. */}
        <label htmlFor={cwdId} className="new-task-form-label">
          Working directory<span className="new-task-form-required"> *</span>
        </label>
        <div className="new-task-form-cwd-row">
          <input
            id={cwdId}
            ref={cwdRef}
            type="text"
            className="new-task-form-input new-task-form-cwd-input"
            value={cwd}
            onChange={(e) => setCwd(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') {
                e.preventDefault();
                void handleSubmit();
              }
            }}
            placeholder="/Users/you/code/project"
            aria-invalid={cwdError !== null}
            aria-describedby={cwdError ? `${cwdId}-err` : undefined}
            required
          />
          {/* Browse… opens the directory walker. Always pushes into the
              surrounding Dialog via `useModalView()` — NewTaskForm is
              hosted inside a Dialog in every in-app caller (NewWaveCTA
              in Cove.tsx). The typed input above remains the source of
              truth — Browse is just a shortcut that *sets* the cwd, it
              doesn't replace the field. Accessible name comes from the
              visible text ("Browse…") so
              `getByLabel(/working directory/i)` on the surrounding
              field still uniquely resolves to the cwd input; `title`
              carries the contextual hint for sighted users and matches
              the SR-description for screen readers without colliding
              with the field's label text. */}
          <button
            type="button"
            className="new-task-form-cwd-browse"
            onClick={startBrowse}
            title="Browse for working directory"
          >
            Browse…
          </button>
        </div>
        {cwdError && (
          <p id={`${cwdId}-err`} className="new-task-form-fielderr">
            {cwdError}
          </p>
        )}

        {/* Cove section — three render branches keyed on resolveState +
            coveChoice. The label text stays "Cove" across all branches
            so the visual structure doesn't jitter. */}
        <CoveSection
          coveSelectId={coveSelectId}
          newCoveNameId={newCoveNameId}
          resolveState={resolveState}
          coveChoice={coveChoice}
          setCoveChoice={setCoveChoice}
          coves={coves}
          defaultCoveId={defaultCoveId}
          seededPaletteColor={seededPaletteColor}
          onOverrideAutoMatch={() => {
            // User wants to override the auto-match. Switch back to the
            // miss-mode picker (existing cove default → defaultCoveId,
            // else first cove, else fall through to "new"), and latch
            // the override flag so subsequent cwd resolves don't
            // clobber the manual pick (resolveState banner can still
            // update, but coveChoice stays).
            userOverrodeAutoMatchRef.current = true;
            const fallbackExistingId =
              defaultCoveId ?? coves[0]?.id ?? '';
            if (fallbackExistingId) {
              setCoveChoice({ mode: 'existing', coveId: fallbackExistingId });
            } else {
              setCoveChoice({
                mode: 'new',
                name: '',
                color: seededPaletteColor(),
              });
            }
          }}
        />

        {errorMsg && (
          <p className="new-task-form-err" role="alert">
            {errorMsg}
          </p>
        )}

        <div className="new-task-form-actions">
          <button
            type="button"
            className="new-task-form-cancel"
            onClick={onCancel}
          >
            Cancel
          </button>
          <button
            type="submit"
            className="new-task-form-submit"
            disabled={!canSubmit}
            // Some screen readers prefer aria-disabled over the
            // native disabled attribute (which silently swallows focus
            // and keystrokes). Both are set; the visual / pointer
            // behaviour comes from native, the AT exposure from aria.
            aria-disabled={!canSubmit}
          >
            {submitting ? 'Creating…' : 'Create task'}
          </button>
        </div>
      </form>
    </section>
  );
}

// ---------------------------------------------------------------------------
// Cove section — branch on resolveState
// ---------------------------------------------------------------------------

function CoveSection({
  coveSelectId,
  newCoveNameId,
  resolveState,
  coveChoice,
  setCoveChoice,
  coves,
  defaultCoveId,
  seededPaletteColor,
  onOverrideAutoMatch,
}: {
  coveSelectId: string;
  newCoveNameId: string;
  resolveState:
    | { kind: 'idle' }
    | { kind: 'resolving' }
    | { kind: 'hit'; resolve: CoveResolveBody }
    | { kind: 'miss' };
  coveChoice: CoveChoice;
  setCoveChoice: (next: CoveChoice) => void;
  coves: { id: string; name: string }[];
  defaultCoveId?: string;
  seededPaletteColor: () => string;
  onOverrideAutoMatch: () => void;
}) {
  // The "auto-matched" branch only renders when the parent's
  // coveChoice is still in auto-mode AND the resolve hit. Once the
  // user clicks "Use a different cove", coveChoice flips to
  // existing/new and we fall through to the radio picker below — the
  // banner still shows what the cwd matches via `resolveState.kind`
  // but it's no longer locked.
  if (resolveState.kind === 'hit' && coveChoice.mode === 'auto') {
    const matched = coves.find((c) => c.id === resolveState.resolve.cove_id);
    return (
      <div className="new-task-form-cove">
        <p className="new-task-form-label">Cove</p>
        <p className="new-task-form-cove-auto" data-testid="cove-auto-match">
          Auto-matched to cove{' '}
          <strong>{matched?.name ?? resolveState.resolve.cove_id}</strong>{' '}
          (via folder <code>{resolveState.resolve.folder_path}</code>).{' '}
          <button
            type="button"
            className="new-task-form-cove-override"
            onClick={onOverrideAutoMatch}
          >
            Use a different cove
          </button>
        </p>
      </div>
    );
  }
  if (resolveState.kind === 'resolving') {
    return (
      <div className="new-task-form-cove">
        <p className="new-task-form-label">Cove</p>
        <p className="new-task-form-cove-resolving">Looking up cove…</p>
      </div>
    );
  }
  // idle / miss — user picks. "idle" is the pre-typing state; we still
  // surface the picker so the user can decide ahead of typing a cwd
  // (and the cwd remains the source of truth for whether attach_folder
  // kicks in at submit time).
  const mode: 'existing' | 'new' =
    coveChoice.mode === 'existing'
      ? 'existing'
      : coveChoice.mode === 'new'
        ? 'new'
        : 'existing';
  return (
    <div className="new-task-form-cove">
      <label htmlFor={coveSelectId} className="new-task-form-label">
        Cove<span className="new-task-form-required"> *</span>
      </label>
      <div
        role="radiogroup"
        aria-label="Cove selection"
        className="new-task-form-cove-modes"
      >
        <label className="new-task-form-cove-mode">
          <input
            type="radio"
            name="cove-mode"
            value="existing"
            checked={mode === 'existing'}
            onChange={() =>
              setCoveChoice({
                mode: 'existing',
                coveId:
                  (coveChoice.mode === 'existing' && coveChoice.coveId) ||
                  defaultCoveId ||
                  coves[0]?.id ||
                  '',
              })
            }
            disabled={coves.length === 0}
          />
          Existing cove
        </label>
        <label className="new-task-form-cove-mode">
          <input
            type="radio"
            name="cove-mode"
            value="new"
            checked={mode === 'new'}
            onChange={() =>
              setCoveChoice({
                mode: 'new',
                name: coveChoice.mode === 'new' ? coveChoice.name : '',
                color:
                  coveChoice.mode === 'new'
                    ? coveChoice.color
                    : seededPaletteColor(),
              })
            }
          />
          Create new cove
        </label>
      </div>
      {mode === 'existing' && coves.length > 0 ? (
        <select
          id={coveSelectId}
          className="new-task-form-input"
          value={coveChoice.mode === 'existing' ? coveChoice.coveId : ''}
          onChange={(e) => setCoveChoice({ mode: 'existing', coveId: e.target.value })}
        >
          {coves.map((c) => (
            <option key={c.id} value={c.id}>
              {c.name}
            </option>
          ))}
        </select>
      ) : mode === 'existing' ? (
        <p className="new-task-form-cove-resolving">
          No coves yet — switch to “Create new cove” above.
        </p>
      ) : (
        <input
          id={newCoveNameId}
          type="text"
          className="new-task-form-input"
          value={coveChoice.mode === 'new' ? coveChoice.name : ''}
          onChange={(e) =>
            setCoveChoice({
              mode: 'new',
              name: e.target.value,
              color:
                coveChoice.mode === 'new'
                  ? coveChoice.color
                  : seededPaletteColor(),
            })
          }
          placeholder="New cove name"
          aria-label="New cove name"
        />
      )}
    </div>
  );
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function isAbsolutePath(p: string): boolean {
  return p.length > 0 && p.startsWith('/');
}

/**
 * Pick a palette color deterministically by cycling through
 * `COVE_PALETTE` indexed by the caller's seed (current cove count is
 * the natural choice). Using `Math.random` here would (a) make tests
 * flaky and (b) jitter the color each render for the same UI state.
 * The seed value is opaque — any non-negative integer works. Negative
 * or non-integer seeds clamp to 0.
 */
function pickPaletteColor(seed: number): string {
  const idx = Number.isFinite(seed) && seed >= 0
    ? Math.floor(seed) % COVE_PALETTE.length
    : 0;
  return COVE_PALETTE[idx];
}

function canSubmitForm({
  cwd,
  cwdError,
  coveChoice,
  submitting,
}: {
  cwd: string;
  cwdError: string | null;
  coveChoice: CoveChoice;
  submitting: boolean;
}): boolean {
  if (submitting) return false;
  if (!isAbsolutePath(cwd.trim())) return false;
  if (cwdError) return false;
  if (coveChoice.mode === 'existing' && !coveChoice.coveId) return false;
  if (coveChoice.mode === 'new' && !coveChoice.name.trim()) return false;
  return true;
}

function readHostThemeRgb() {
  if (typeof document === 'undefined') return DARK_THEME_RGB;
  return document.documentElement.dataset.theme === 'light'
    ? LIGHT_THEME_RGB
    : DARK_THEME_RGB;
}

/**
 * Translate kernel errors into something a user can act on. The
 * server's 409 body for folder conflicts carries enough structure to
 * say *what* collided and *where*; pre-CalmApiError-rewrite this was
 * just the raw string.
 *
 * `coves` is the current `useCovesQuery` snapshot — we look up the
 * conflicting cove's display name from `body.cove_id` so the user
 * sees "claimed by cove **Atlas**" instead of an opaque UUID. If the
 * cove isn't in our local cache (e.g. it was created in a sibling tab
 * and our coves-query hasn't refreshed, or it was deleted between the
 * conflict-detect and our error render), we fall back to the generic
 * "another cove" phrasing.
 */
function formatSubmitError(
  err: unknown,
  coves: { id: string; name: string }[],
): string {
  if (!(err instanceof CalmApiError)) {
    if (err instanceof Error) return err.message;
    return 'Failed to create task.';
  }
  if (err.status === 409) {
    const body = asFolderConflict(err.body);
    if (body) {
      const conflicting = coves.find((c) => c.id === body.cove_id);
      // React's default text escaping handles the cove name when it
      // renders, but the message is a plain string here — the caller
      // drops it into a <p> via `{errorMsg}`, which also escapes. No
      // raw HTML path.
      const coveLabel = conflicting
        ? `cove “${conflicting.name}”`
        : 'another cove';
      switch (body.conflict_kind) {
        case 'descendant':
          return `That path is already claimed by ${coveLabel} (folder \`${body.conflict_path}\`). Pick that cove or choose a different path.`;
        case 'ancestor':
          return `An existing narrower claim under \`${body.conflict_path}\` (owned by ${coveLabel}) blocks claiming this directory. Remove the inner claim first or pick a different path.`;
        case 'equal':
          return `That exact path is already claimed by ${coveLabel} (folder \`${body.conflict_path}\`).`;
      }
    }
    return err.message || 'Path conflict.';
  }
  if (err.status === 422) {
    return 'Missing required field — check the form values and try again.';
  }
  if (err.status === 400) {
    return err.message || 'Bad request.';
  }
  return err.message || 'Failed to create task.';
}

/**
 * Narrow `CalmApiError.body` (which is `unknown` so the wire types
 * don't leak everywhere) to a FolderConflict shape. `null` when the
 * server returned some other error body; the caller falls back to the
 * raw message string.
 */
function asFolderConflict(body: unknown): FolderConflictBody | null {
  if (
    body &&
    typeof body === 'object' &&
    'conflict_path' in body &&
    typeof (body as { conflict_path: unknown }).conflict_path === 'string' &&
    'conflict_kind' in body &&
    'cove_id' in body &&
    typeof (body as { cove_id: unknown }).cove_id === 'string'
  ) {
    const kind = (body as { conflict_kind: unknown }).conflict_kind;
    if (kind === 'descendant' || kind === 'ancestor' || kind === 'equal') {
      return body as FolderConflictBody;
    }
  }
  return null;
}
