// ---------------- NewTaskForm ----------------
//
// Issue #250 PR 3 — the single creation surface for "task = description
// + cwd + area". Used by the area page's `+ New track` affordance today,
// the calendar's empty-cell click later (PR 6). Per the issue comment
// "all creation entrypoints must go through the same configuration
// card", this component is the only place that knows how to POST a
// well-formed `NewTrack` body — every other entrypoint must reuse it
// (not re-implement the cwd/area inference).
//
// Field semantics (decided across #255 + #250 PR 2, updated by #409):
//   * task description (optional) → posted as `track.title`. The kernel
//     threads a non-empty title into the planner daemon as the initial
//     prompt; an empty title boots the planner daemon without an
//     auto-submitted prompt. We deliberately do not surface a separate
//     "prompt" field — title-as-prompt keeps the track-row label and the
//     prompt in lock-step when a prompt exists.
//   * cwd (required) → absolute path the planner daemon spawns under.
//     The form refuses to submit a non-`/`-prefixed value; the server
//     would 400 anyway, but inline rejection is cheaper than a round
//     trip + read of an error toast.
//   * area (required) → derived from cwd via `GET /api/areas/resolve`:
//       - hit  → field locked to the auto-matched area; submit goes
//                straight through (no `attach_folder` opt-in needed,
//                the cwd is already under that area's folder claim).
//       - miss → field user-editable, two paths:
//                  "existing": pick an area from `useAreasQuery`, submit
//                    with `attach_folder: true` so the kernel adds
//                    `cwd` as a new folder under that area inside the
//                    same tx as the track-create. PR 3 implements this
//                    as a two-step client-side flow for the "create
//                    new area + claim cwd" branch (see below) — the
//                    "existing area + attach cwd" branch is a single
//                    POST with `attach_folder: true`, which the kernel
//                    handles atomically inside one tx.
//                  "new":      mint a fresh area (`POST /api/areas`),
//                    then POST the track with the new id + the same
//                    `attach_folder: true` flag. Two-step (area +
//                    track) because the server doesn't yet expose an
//                    atomic "create area + first track" endpoint; if
//                    the track POST fails (e.g. validation), the area
//                    is left in place and a retry reuses it. Followup
//                    todo to collapse this into one atomic endpoint
//                    if the leftover-area cost ever bites.
//
// 409 / FolderConflict handling: NewTrack's `attach_folder: true` path
// can land a structured 409 on this list of scenarios:
//   - cwd is descendant of a folder owned by a *different* area (the
//     resolve would have warned us pre-submit, but a concurrent claim
//     can still race here);
//   - cwd is an ancestor of an existing narrower claim (widening, the
//     server refuses for resolution ambiguity).
// The form reads the `{area_id, conflict_path, conflict_kind}` body
// and renders a one-line, user-readable diagnosis without leaking the
// raw enum into the UI.
//
// A11y: every input has a real <label> (htmlFor + id); the wrapping
// section is `role="form"` with a labelled heading so a Playwright
// `getByRole('form', { name: 'New task' })` lookup is unambiguous in
// dense pages (Area page below + calendar later).

import { useCallback, useEffect, useId, useMemo, useRef } from 'react';
import type { RefObject } from 'react';
import { useState } from '../state';
import { useQueryClient } from '@tanstack/react-query';
import * as api from '../../api/calm';
import { CalmApiError } from '../../api/calm';
import {
  queryKeys,
  useAreasQuery,
  useCreateAreaMutation,
  useCreateTrackMutation,
} from '../../api/queries';
import { DARK_THEME_RGB, LIGHT_THEME_RGB } from '../../api/themeRgb';
import type { AreaResolveBody, FolderConflictBody, KernelTrack } from '../../api/wire';
import { ChevronIcon } from './ChevronIcon';
import { DirectoryBrowser } from './DirectoryPicker';
import { parseGitHubIssueUrl } from './issueUrl';
import { useModalView } from '../../ui/Dialog/Dialog';

/** Result handed back to the caller on successful POST `/api/tracks`. */
export type NewTaskFormResult = KernelTrack;

export interface NewTaskFormProps {
  /** Pre-selected area. When the surrounding page already scopes itself
   *  to an area (area page) we pass it here so the dropdown defaults to
   *  it on first paint. The cwd-resolve auto-match still overrides this
   *  if it lands a different area. */
  defaultAreaId?: string;
  /** Fired after the track-create POST succeeds. Caller usually navigates
   *  to `/calm/track/<id>`. */
  onCreated: (track: NewTaskFormResult) => void | Promise<void>;
  /** Fired when the user dismisses the form (Esc, Cancel). Caller
   *  collapses the inline panel back to a CTA button. */
  onCancel: () => void;
  /** Optional ref the form binds to its variant's FIRST field — the
   *  title textarea for `'task'`, the GitHub issue URL input for
   *  `'issue-dev'` (#891 review: initial focus must be
   *  variant-appropriate). When provided, the caller (typically a host
   *  `<Dialog>`) uses this to claim initial focus on that field — the
   *  form skips its own `queueMicrotask(focus)` mount effect to avoid
   *  racing against the Dialog's rAF "focus first focusable" pass,
   *  which otherwise lands focus on the Dialog's Close button. When
   *  omitted, the form falls back to focusing its first field itself
   *  on mount. Typed `HTMLElement` (not a concrete element type)
   *  because which element it points at is variant-dependent; Dialog's
   *  own `initialFocusRef` is `HTMLElement` too. */
  initialFocusRef?: RefObject<HTMLElement | null>;
  /** Issue #891 slice ③ — form variant. `'task'` (default) is the plain
   *  track path, untouched. `'issue-dev'` binds the created track to the
   *  shipped `issue-development` workflow: the user supplies a GitHub
   *  issue URL (plus the same cwd/area flow), the form derives
   *  `{repo, issue_number, issue_url}` client-side and POSTs them as
   *  `template_input` alongside `template_id: "issue-development"`. */
  variant?: 'task' | 'issue-dev';
}

/** #891 (design §6 F5) — this legacy form deliberately does not consume
 *  `GET /api/track-templates`, so the issue-dev variant hardcodes the id.
 *  If the git-forge plugin isn't running, the create POST 400s with a
 *  readable message that the normal submit-error path surfaces. */
const ISSUE_DEV_TEMPLATE_ID = 'issue-development';

/** Allowed `template_input.merge_policy` values — mirrors the enum in the
 *  shipped `issue-development` input_schema (git-forge manifest, #891 ②).
 *  The default mirrors the schema's documentary `default`: kernel doesn't
 *  apply defaults (design F6), so the form always sends the value. The
 *  policy is binary, so the UI is an "Auto-merge" checkbox (#891 signoff
 *  r3 — kills the last OS dropdown popup): unchecked = 'hold-for-ratify',
 *  checked = 'auto-merge'. The wire shape is unchanged. */
type MergePolicy = 'hold-for-ratify' | 'auto-merge';

/** Debounce window for the cwd → resolve API call. 300ms balances
 *  "feels live" against "didn't fire a request after every keypress". */
const RESOLVE_DEBOUNCE_MS = 300;

/** Fallback palette for the "create new area" branch — same set
 *  Sidebar's `NewAreaButton` draws from. Keep in lockstep; a real
 *  color picker is a future enhancement. */
const AREA_PALETTE = ['#5a9', '#c97', '#79c', '#b86', '#6a8', '#a6c'];

type AreaChoice =
  | { mode: 'auto'; resolve: AreaResolveBody }
  | { mode: 'existing'; areaId: string }
  | { mode: 'new'; name: string; color: string };

export function NewTaskForm({
  defaultAreaId,
  onCreated,
  onCancel,
  initialFocusRef,
  variant = 'task',
}: NewTaskFormProps) {
  const titleId = useId();
  const cwdId = useId();
  const areaSelectId = useId();
  const newAreaNameId = useId();
  const headingId = useId();
  const issueUrlId = useId();
  const mergePolicyId = useId();
  const rawJsonId = useId();

  const [title, setTitle] = useState('');
  const [cwd, setCwd] = useState('');
  // ---- issue-dev variant state (#891 ③). Inert in the 'task' variant:
  // nothing below reads it and the submit body spreads nothing in.
  const isIssueDev = variant === 'issue-dev';
  const [issueUrl, setIssueUrl] = useState('');
  const [mergePolicy, setMergePolicy] = useState<MergePolicy>('hold-for-ratify');
  // Raw JSON escape hatch: `null` = not overridden (the textarea mirrors
  // the derived template_input); a string = the user has taken over and
  // their JSON is what gets POSTed (schema-level validation stays
  // server-side — a 400 surfaces through the normal error path).
  const [rawJson, setRawJson] = useState<string | null>(null);
  // Title prefill ("dev #<n>") only runs while the user hasn't typed a
  // title of their own — one manual edit latches the field.
  const titleEditedRef = useRef(false);
  const [resolveState, setResolveState] = useState<
    | { kind: 'idle' }
    | { kind: 'resolving' }
    | { kind: 'hit'; resolve: AreaResolveBody }
    | { kind: 'miss' }
  >({ kind: 'idle' });
  // When the resolve misses, the user picks between "existing area" and
  // "new area". Default to "existing" if a `defaultAreaId` was passed
  // (caller already has one in mind); otherwise "new" — the user
  // typed a cwd nobody owns, "create an area for this" is the obvious
  // next step.
  const areasQ = useAreasQuery();
  const areas = useMemo(() => areasQ.data ?? [], [areasQ.data]);

  // Deterministic palette seed: cycle through AREA_PALETTE by the
  // current area count so the "Create new area" branch picks a stable
  // color for the same UI state (no Math.random flake in tests, no
  // jitter between renders for the same user state).
  const seededPaletteColor = useCallback(
    () => pickPaletteColor(areas.length),
    [areas.length],
  );

  const [areaChoice, setAreaChoice] = useState<AreaChoice>(() =>
    defaultAreaId
      ? { mode: 'existing', areaId: defaultAreaId }
      : { mode: 'new', name: '', color: pickPaletteColor(0) },
  );
  const [submitting, setSubmitting] = useState(false);
  const [errorMsg, setErrorMsg] = useState<string | null>(null);
  // Tracks whether the user has explicitly overridden an auto-match. A
  // hit lands → we set areaChoice to `{ mode: 'auto', ... }` AND clear
  // this flag so future resolves can also auto-match. Once the user
  // clicks "Use a different area", we set this flag; subsequent
  // resolves still update resolveState (so the banner can still
  // describe what the cwd matches), but they no longer overwrite the
  // user's manual areaChoice.
  const userOverrodeAutoMatchRef = useRef(false);

  const createTrack = useCreateTrackMutation();
  const createArea = useCreateAreaMutation();
  const qc = useQueryClient();
  // Browse… → always pushes a DirectoryBrowser view into the surrounding
  // Dialog's body via `useModalView()`. NewTaskForm is hosted exclusively
  // inside a Dialog today (NewTrackCTA in Area.tsx wraps it), so the
  // modal-view context is always present in production. The dev-time
  // `console.warn` below catches accidental Dialog-less renderings during
  // refactors instead of silently breaking the Browse affordance.
  const modalView = useModalView();

  const localTitleRef = useRef<HTMLTextAreaElement | null>(null);
  const urlInputRef = useRef<HTMLInputElement | null>(null);
  // The variant's FIRST field carries `initialFocusRef` (when the caller
  // forwarded one) so the host Dialog's initial-focus pass — and the
  // caller's variant-change refocus — land on the right element: the
  // issue-URL input in 'issue-dev', the title textarea in 'task'.
  // Callback refs bridge the typing (the shared ref is `HTMLElement`,
  // the elements are concrete input/textarea types — a plain ref-object
  // handoff would trip TS property variance). `isIssueDev` is fixed for
  // the lifetime of a mount (Area.tsx remounts the form via `key` on
  // variant switch), so each callback binds at most one element.
  const titleFieldRef = useCallback(
    (el: HTMLTextAreaElement | null) => {
      localTitleRef.current = el;
      if (!isIssueDev && initialFocusRef) initialFocusRef.current = el;
    },
    [isIssueDev, initialFocusRef],
  );
  const urlFieldRef = useCallback(
    (el: HTMLInputElement | null) => {
      urlInputRef.current = el;
      if (isIssueDev && initialFocusRef) initialFocusRef.current = el;
    },
    [isIssueDev, initialFocusRef],
  );
  const cwdRef = useRef<HTMLInputElement | null>(null);
  // Focus the variant's first field on mount — opening the form should
  // land the caret in the first meaningful input without an extra
  // click. Skipped when the caller forwarded `initialFocusRef`: the
  // Dialog's own rAF-deferred focus pass would race against this
  // microtask and sometimes win (landing on the Dialog Close button),
  // so the contract is "Dialog focuses for us, we don't double-focus".
  useEffect(() => {
    if (initialFocusRef) return;
    queueMicrotask(() =>
      (isIssueDev ? urlInputRef.current : localTitleRef.current)?.focus(),
    );
  }, [initialFocusRef, isIssueDev]);

  // Latest cwd at commit-time. The resolve effect captures `cwd` via
  // closure, but the in-flight `api.resolveAreaPath` Promise may resolve
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
          const hit = await api.resolveAreaPath(trimmed);
          // Race guard: drop the result if the user has typed past this
          // cwd since the request fired. Without this check, two
          // overlapping resolves can land out-of-order and the stale
          // one wins.
          if (latestResolveCwdRef.current !== trimmed) return;
          if (hit) {
            setResolveState({ kind: 'hit', resolve: hit });
            // Once a hit lands, the area choice is forced — unless the
            // user has explicitly overridden a previous auto-match, in
            // which case the banner still updates (so the user sees
            // what the cwd matches) but the manual areaChoice stands.
            if (!userOverrodeAutoMatchRef.current) {
              setAreaChoice({ mode: 'auto', resolve: hit });
            }
          } else {
            setResolveState({ kind: 'miss' });
            // On miss, fall back to the default areaChoice that was
            // seeded at mount — but only if we're currently in `auto`
            // (a previous hit). Otherwise the user's pick stands.
            setAreaChoice((cur) =>
              cur.mode === 'auto'
                ? defaultAreaId
                  ? { mode: 'existing', areaId: defaultAreaId }
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
          // user can still pick / create an area. The submit path will
          // re-validate via the server.
          setResolveState({ kind: 'miss' });
          // Keep the inline error visible if the resolve failed mid-
          // typing; the user can still proceed via manual area pick.
          if (e instanceof CalmApiError && e.status !== 400) {
            setErrorMsg(`Path lookup failed: ${e.message}`);
          }
        }
      })();
    }, RESOLVE_DEBOUNCE_MS);
    return () => clearTimeout(timer);
  }, [cwd, defaultAreaId, seededPaletteColor]);

  const cwdError = cwd.length > 0 && !isAbsolutePath(cwd.trim())
    ? 'Path must be absolute (start with `/`).'
    : null;

  // ---- issue-dev derivations (#891 ③) -------------------------------
  const parsedIssue = useMemo(
    () => (isIssueDev ? parseGitHubIssueUrl(issueUrl) : null),
    [isIssueDev, issueUrl],
  );
  const issueUrlError =
    isIssueDev && issueUrl.trim().length > 0 && !parsedIssue
      ? 'Must be a GitHub issue URL (https://github.com/owner/repo/issues/123).'
      : null;

  // Prefill title as `dev #<n>` once the URL parses. Editable: a manual
  // title edit latches `titleEditedRef` and the prefill stops following
  // the URL. `setTitle` from here doesn't fire the textarea's onChange,
  // so the latch only trips on real user input.
  useEffect(() => {
    if (!parsedIssue || titleEditedRef.current) return;
    setTitle(`dev #${parsedIssue.issue_number}`);
  }, [parsedIssue]);

  /** The `template_input` derived from the structured fields —
   *  merge_policy always present. `null` until the URL parses. The
   *  schema's optional `notes` key has no form field (#891 signoff:
   *  it duplicated the task-description free-text) — the raw-JSON
   *  escape hatch is the way to send one. */
  const derivedTemplateInput = useMemo(() => {
    if (!parsedIssue) return null;
    return {
      issue_url: parsedIssue.issue_url,
      repo: parsedIssue.repo,
      issue_number: parsedIssue.issue_number,
      merge_policy: mergePolicy,
    };
  }, [parsedIssue, mergePolicy]);

  // What the raw-JSON textarea shows: the user's override once they've
  // edited, otherwise a live mirror of the derived input.
  const rawJsonText =
    rawJson ?? (derivedTemplateInput ? JSON.stringify(derivedTemplateInput, null, 2) : '');
  const rawJsonError = useMemo(() => {
    if (rawJson === null) return null;
    try {
      JSON.parse(rawJson);
      return null;
    } catch (e) {
      return e instanceof Error ? e.message : 'not valid JSON';
    }
  }, [rawJson]);

  const canSubmit = canSubmitForm({
    cwd,
    cwdError,
    areaChoice,
    submitting,
    issueDev: isIssueDev
      ? {
          parsedOk: parsedIssue !== null,
          rawOverride: rawJson !== null,
          rawValid: rawJsonError === null,
        }
      : null,
  });

  const handleSubmit = useCallback(
    async (e?: React.FormEvent | React.KeyboardEvent) => {
      e?.preventDefault();
      if (!canSubmit) return;
      setSubmitting(true);
      setErrorMsg(null);
      try {
        const finalCwd = cwd.trim();
        // Resolve the area_id + attach_folder flag from the form state:
        //   * auto → cwd already covered; attach=false
        //   * existing → user-picked; attach=true so the cwd lands as a
        //     folder under that area inside the track-create tx.
        //   * new → mint the area first, then submit the track under it
        //     with attach=true.
        let areaId: string;
        let attachFolder: boolean;
        if (areaChoice.mode === 'auto') {
          areaId = areaChoice.resolve.area_id;
          attachFolder = false;
        } else if (areaChoice.mode === 'existing') {
          areaId = areaChoice.areaId;
          attachFolder = true;
        } else {
          // Two-step: area first, then track. If the track POST fails
          // the area is left in place — see file header for rationale.
          // TODO(#250): atomic create-area-and-track endpoint to collapse
          // this two-step and remove the leftover-area risk on partial
          // failure (current fallback: a retry reuses the orphan area).
          const area = await createArea.mutateAsync({
            name: areaChoice.name.trim(),
            color: areaChoice.color,
          });
          areaId = area.id;
          attachFolder = true;
          // The new area is already in `useAreasQuery` cache via the
          // mutation's onSuccess invalidate. No extra work here.
        }

        // issue-dev variant (#891 ③): bind the track to the shipped
        // workflow and carry the input JSON. The raw-JSON override wins
        // when the user has edited it (canSubmit already gated on it
        // parsing); otherwise the derived structured fields go out. The
        // plain 'task' variant spreads nothing — its body stays
        // byte-identical to pre-#891.
        const templateFields = isIssueDev
          ? {
              template_id: ISSUE_DEV_TEMPLATE_ID,
              template_input:
                rawJson !== null ? (JSON.parse(rawJson) as unknown) : derivedTemplateInput,
            }
          : {};
        const track = await createTrack.mutateAsync({
          area_id: areaId,
          title: title.trim(),
          cwd: finalCwd,
          attach_folder: attachFolder,
          theme: readHostThemeRgb(),
          ...templateFields,
        });
        // Belt-and-suspenders cache invalidate — useCreateTrackMutation
        // already kicks ['tracks', area_id], but a brand-new area also
        // benefits from a areas-list refresh in case the WS event
        // didn't land yet.
        void qc.invalidateQueries({ queryKey: queryKeys.areas() });
        await onCreated(track);
      } catch (e) {
        const formatted = formatSubmitError(e, areas);
        setErrorMsg(formatted);
      } finally {
        setSubmitting(false);
      }
    },
    [
      canSubmit,
      areaChoice,
      areas,
      createArea,
      createTrack,
      cwd,
      derivedTemplateInput,
      isIssueDev,
      onCreated,
      qc,
      rawJson,
      title,
    ],
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
        {isIssueDev ? 'New issue-dev task' : 'New task'}
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
        {/* Issue URL — issue-dev variant only (#891 ③). The one
            user-facing required field besides cwd: everything else in
            the template_input is derived from it client-side
            (parseGitHubIssueUrl). Inline error + disabled submit on a
            malformed URL, same pattern as the cwd field below. */}
        {isIssueDev && (
          <>
            <label htmlFor={issueUrlId} className="new-task-form-label">
              GitHub issue URL<span className="new-task-form-required"> *</span>
            </label>
            <input
              id={issueUrlId}
              ref={urlFieldRef}
              type="text"
              className="new-task-form-input"
              value={issueUrl}
              onChange={(e) => setIssueUrl(e.target.value)}
              placeholder="https://github.com/owner/repo/issues/123"
              aria-invalid={issueUrlError !== null}
              aria-describedby={issueUrlError ? `${issueUrlId}-err` : undefined}
              required
            />
            {issueUrlError && (
              <p id={`${issueUrlId}-err`} className="new-task-form-fielderr">
                {issueUrlError}
              </p>
            )}
          </>
        )}

        {/* Task description ↔ track.title. Textarea so the user can
            paste a multi-line ask without us truncating. Enter is
            *not* submit here — newlines in the description are
            valid; submit is the explicit "Create task" button.
            Empty is also valid: the planner daemon boots with no
            auto-submitted prompt. In the issue-dev variant it's
            prefilled `dev #<n>` from the parsed URL; a manual edit
            latches it (titleEditedRef). */}
        <label htmlFor={titleId} className="new-task-form-label">
          Task description
        </label>
        <textarea
          id={titleId}
          ref={titleFieldRef}
          className="new-task-form-input"
          rows={3}
          value={title}
          onChange={(e) => {
            titleEditedRef.current = true;
            setTitle(e.target.value);
          }}
          placeholder="What should the agent do?"
        />

        {/* Merge policy — issue-dev variant only, surfaced as an
            "Auto-merge" checkbox because the policy is binary (#891
            signoff r3; a <select> here was the last OS popup on the
            card). Unchecked (default) = 'hold-for-ratify', checked =
            'auto-merge'; the derived merge_policy is always sent
            (kernel doesn't apply schema defaults, design F6). Native
            checkbox — the app has no switch primitive, and a real
            <input type="checkbox"> needs no custom aria; the one-line
            hint is wired as its accessible description. The schema's
            optional `notes` deliberately has no field here (#891
            signoff: it duplicated the task-description free-text);
            the raw-JSON escape hatch below still carries it. */}
        {isIssueDev && (
          <div className="new-task-form-automerge">
            <label
              htmlFor={mergePolicyId}
              className="new-task-form-automerge-label"
            >
              <input
                id={mergePolicyId}
                type="checkbox"
                className="new-task-form-automerge-box"
                checked={mergePolicy === 'auto-merge'}
                onChange={(e) =>
                  setMergePolicy(e.target.checked ? 'auto-merge' : 'hold-for-ratify')
                }
                aria-describedby={`${mergePolicyId}-hint`}
              />
              Auto-merge
            </label>
            <p
              id={`${mergePolicyId}-hint`}
              className="new-task-form-automerge-hint"
            >
              Off, the merge waits for your approval; on, it merges
              automatically once the fence converges and checks are green.
            </p>
          </div>
        )}

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
              hosted inside a Dialog in every in-app caller (NewTrackCTA
              in Area.tsx). The typed input above remains the source of
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

        {/* Area section — three render branches keyed on resolveState +
            areaChoice. The label text stays "Area" across all branches
            so the visual structure doesn't jitter. */}
        <AreaSection
          areaSelectId={areaSelectId}
          newAreaNameId={newAreaNameId}
          resolveState={resolveState}
          areaChoice={areaChoice}
          setAreaChoice={setAreaChoice}
          areas={areas}
          defaultAreaId={defaultAreaId}
          seededPaletteColor={seededPaletteColor}
          onOverrideAutoMatch={() => {
            // User wants to override the auto-match. Switch back to the
            // miss-mode picker (existing area default → defaultAreaId,
            // else first area, else fall through to "new"), and latch
            // the override flag so subsequent cwd resolves don't
            // clobber the manual pick (resolveState banner can still
            // update, but areaChoice stays).
            userOverrodeAutoMatchRef.current = true;
            const fallbackExistingId =
              defaultAreaId ?? areas[0]?.id ?? '';
            if (fallbackExistingId) {
              setAreaChoice({ mode: 'existing', areaId: fallbackExistingId });
            } else {
              setAreaChoice({
                mode: 'new',
                name: '',
                color: seededPaletteColor(),
              });
            }
          }}
        />

        {/* Raw JSON escape hatch — issue-dev variant only (#891 ③,
            design §3.1). Collapsed by default; shows the exact
            `template_input` the form will POST (live-derived from the
            fields above until the user edits, at which point the raw
            value takes over). Local JSON.parse gates submit; the
            schema-level validation stays server-side and a 400 lands in
            the normal error alert below. This is the generic seam:
            future templates get raw mode first, a thin form later. */}
        {isIssueDev && (
          <details className="new-task-form-rawjson">
            {/* The override state is surfaced on the always-visible
                <summary>, not just inside the collapsible body: once the
                user edits the raw JSON and collapses the section, a stale
                raw blob would otherwise ship with no visible indicator
                that the form fields above are being ignored. */}
            <summary className="new-task-form-rawjson-summary">
              Raw template_input JSON
              {rawJson !== null ? ' — overriding form fields' : ''}
            </summary>
            <textarea
              aria-label="Raw template_input JSON"
              className="new-task-form-input new-task-form-rawjson-text"
              rows={8}
              spellCheck={false}
              value={rawJsonText}
              onChange={(e) => setRawJson(e.target.value)}
              aria-invalid={rawJsonError !== null}
              // While the JSON is invalid, AT users hear the parse error
              // as the textarea's description. The error <p> stays a
              // plain paragraph — role="alert" is reserved for the
              // submit/server error surface below.
              aria-describedby={rawJsonError !== null ? `${rawJsonId}-err` : undefined}
            />
            {/* Reset renders whenever raw mode is active — including
                while the JSON is malformed or the textarea is empty.
                Gating it on validity would strand the user: the only way
                out of a broken raw edit would be hand-repairing the
                JSON. */}
            {rawJson !== null && (
              <p className="new-task-form-rawjson-hint">
                Raw JSON overrides the fields above.{' '}
                <button
                  type="button"
                  className="new-task-form-rawjson-reset"
                  onClick={() => setRawJson(null)}
                >
                  Reset to form values
                </button>
              </p>
            )}
            {rawJsonError && (
              <p id={`${rawJsonId}-err`} className="new-task-form-fielderr">
                Invalid JSON: {rawJsonError}
              </p>
            )}
          </details>
        )}

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
// Area section — branch on resolveState
// ---------------------------------------------------------------------------

function AreaSection({
  areaSelectId,
  newAreaNameId,
  resolveState,
  areaChoice,
  setAreaChoice,
  areas,
  defaultAreaId,
  seededPaletteColor,
  onOverrideAutoMatch,
}: {
  areaSelectId: string;
  newAreaNameId: string;
  resolveState:
    | { kind: 'idle' }
    | { kind: 'resolving' }
    | { kind: 'hit'; resolve: AreaResolveBody }
    | { kind: 'miss' };
  areaChoice: AreaChoice;
  setAreaChoice: (next: AreaChoice) => void;
  areas: { id: string; name: string }[];
  defaultAreaId?: string;
  seededPaletteColor: () => string;
  onOverrideAutoMatch: () => void;
}) {
  // The "auto-matched" branch only renders when the parent's
  // areaChoice is still in auto-mode AND the resolve hit. Once the
  // user clicks "Use a different area", areaChoice flips to
  // existing/new and we fall through to the radio picker below — the
  // banner still shows what the cwd matches via `resolveState.kind`
  // but it's no longer locked.
  if (resolveState.kind === 'hit' && areaChoice.mode === 'auto') {
    const matched = areas.find((c) => c.id === resolveState.resolve.area_id);
    return (
      <div className="new-task-form-area">
        <p className="new-task-form-label">Area</p>
        <p className="new-task-form-area-auto" data-testid="area-auto-match">
          Auto-matched to area{' '}
          <strong>{matched?.name ?? resolveState.resolve.area_id}</strong>{' '}
          (via folder <code>{resolveState.resolve.folder_path}</code>).{' '}
          <button
            type="button"
            className="new-task-form-area-override"
            onClick={onOverrideAutoMatch}
          >
            Use a different area
          </button>
        </p>
      </div>
    );
  }
  if (resolveState.kind === 'resolving') {
    return (
      <div className="new-task-form-area">
        <p className="new-task-form-label">Area</p>
        <p className="new-task-form-area-resolving">Looking up area…</p>
      </div>
    );
  }
  // idle / miss — user picks. "idle" is the pre-typing state; we still
  // surface the picker so the user can decide ahead of typing a cwd
  // (and the cwd remains the source of truth for whether attach_folder
  // kicks in at submit time).
  const mode: 'existing' | 'new' =
    areaChoice.mode === 'existing'
      ? 'existing'
      : areaChoice.mode === 'new'
        ? 'new'
        : 'existing';
  return (
    <div className="new-task-form-area">
      <label htmlFor={areaSelectId} className="new-task-form-label">
        Area<span className="new-task-form-required"> *</span>
      </label>
      <div
        role="radiogroup"
        aria-label="Area selection"
        className="new-task-form-area-modes"
      >
        <label className="new-task-form-area-mode">
          <input
            type="radio"
            name="area-mode"
            value="existing"
            checked={mode === 'existing'}
            onChange={() =>
              setAreaChoice({
                mode: 'existing',
                areaId:
                  (areaChoice.mode === 'existing' && areaChoice.areaId) ||
                  defaultAreaId ||
                  areas[0]?.id ||
                  '',
              })
            }
            disabled={areas.length === 0}
          />
          Existing area
        </label>
        <label className="new-task-form-area-mode">
          <input
            type="radio"
            name="area-mode"
            value="new"
            checked={mode === 'new'}
            onChange={() =>
              setAreaChoice({
                mode: 'new',
                name: areaChoice.mode === 'new' ? areaChoice.name : '',
                color:
                  areaChoice.mode === 'new'
                    ? areaChoice.color
                    : seededPaletteColor(),
              })
            }
          />
          Create new area
        </label>
      </div>
      {mode === 'existing' && areas.length > 0 ? (
        /* .calm-select (#891): the themed base-select drawer, same
           treatment as the Workflow select in the New-track dialog so
           the card has ONE popup system. The custom trigger button
           (selectedcontent + stroke chevron) makes the closed field
           render identically to the Workflow trigger; options are
           plain text, so they just get the row/hover/checkmark
           treatment. Fallback engines ignore the button child and
           keep the OS popup.

           The trigger is intentionally a named-but-non-focusable
           button: the <select> owns focus + semantics (base-select).
           In Chromium the trigger still surfaces in the AX button tree
           named by the cloned option content — i.e. the selected AREA
           NAME. We do NOT aria-hidden it (that risks stripping the
           subtree Chrome announces as the selected value); the area
           name in the button surface is why in-dialog button e2e
           locators must use exact names (a substring /browse/i once
           collided with an area named "E2E browse area"). */
        <select
          id={areaSelectId}
          className="new-task-form-input calm-select"
          value={areaChoice.mode === 'existing' ? areaChoice.areaId : ''}
          onChange={(e) => setAreaChoice({ mode: 'existing', areaId: e.target.value })}
        >
          <button className="calm-select-trigger">
            <selectedcontent className="calm-select-selected" />
            <span className="calm-select-chevron" aria-hidden="true">
              <ChevronIcon />
            </span>
          </button>
          {areas.map((c) => (
            <option key={c.id} value={c.id}>
              {c.name}
            </option>
          ))}
        </select>
      ) : mode === 'existing' ? (
        <p className="new-task-form-area-resolving">
          No areas yet — switch to “Create new area” above.
        </p>
      ) : (
        <input
          id={newAreaNameId}
          type="text"
          className="new-task-form-input"
          value={areaChoice.mode === 'new' ? areaChoice.name : ''}
          onChange={(e) =>
            setAreaChoice({
              mode: 'new',
              name: e.target.value,
              color:
                areaChoice.mode === 'new'
                  ? areaChoice.color
                  : seededPaletteColor(),
            })
          }
          placeholder="New area name"
          aria-label="New area name"
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
 * `AREA_PALETTE` indexed by the caller's seed (current area count is
 * the natural choice). Using `Math.random` here would (a) make tests
 * flaky and (b) jitter the color each render for the same UI state.
 * The seed value is opaque — any non-negative integer works. Negative
 * or non-integer seeds clamp to 0.
 */
function pickPaletteColor(seed: number): string {
  const idx = Number.isFinite(seed) && seed >= 0
    ? Math.floor(seed) % AREA_PALETTE.length
    : 0;
  return AREA_PALETTE[idx];
}

function canSubmitForm({
  cwd,
  cwdError,
  areaChoice,
  submitting,
  issueDev,
}: {
  cwd: string;
  cwdError: string | null;
  areaChoice: AreaChoice;
  submitting: boolean;
  /** `null` in the plain 'task' variant. In 'issue-dev' (#891 ③) the
   *  template_input must be produceable: either the raw-JSON override
   *  is active and parses, or the issue URL parsed into the derived
   *  fields. A valid raw override deliberately unblocks submit even
   *  with an unparsed URL — it's the escape hatch (server-side schema
   *  validation is the final arbiter). */
  issueDev: { parsedOk: boolean; rawOverride: boolean; rawValid: boolean } | null;
}): boolean {
  if (submitting) return false;
  if (!isAbsolutePath(cwd.trim())) return false;
  if (cwdError) return false;
  if (areaChoice.mode === 'existing' && !areaChoice.areaId) return false;
  if (areaChoice.mode === 'new' && !areaChoice.name.trim()) return false;
  if (issueDev) {
    if (issueDev.rawOverride) {
      if (!issueDev.rawValid) return false;
    } else if (!issueDev.parsedOk) {
      return false;
    }
  }
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
 * `areas` is the current `useAreasQuery` snapshot — we look up the
 * conflicting area's display name from `body.area_id` so the user
 * sees "claimed by area **Atlas**" instead of an opaque UUID. If the
 * area isn't in our local cache (e.g. it was created in a sibling tab
 * and our areas-query hasn't refreshed, or it was deleted between the
 * conflict-detect and our error render), we fall back to the generic
 * "another area" phrasing.
 */
function formatSubmitError(
  err: unknown,
  areas: { id: string; name: string }[],
): string {
  if (!(err instanceof CalmApiError)) {
    if (err instanceof Error) return err.message;
    return 'Failed to create task.';
  }
  if (err.status === 409) {
    const body = asFolderConflict(err.body);
    if (body) {
      const conflicting = areas.find((c) => c.id === body.area_id);
      // React's default text escaping handles the area name when it
      // renders, but the message is a plain string here — the caller
      // drops it into a <p> via `{errorMsg}`, which also escapes. No
      // raw HTML path.
      const areaLabel = conflicting
        ? `area “${conflicting.name}”`
        : 'another area';
      switch (body.conflict_kind) {
        case 'descendant':
          return `That path is already claimed by ${areaLabel} (folder \`${body.conflict_path}\`). Pick that area or choose a different path.`;
        case 'ancestor':
          return `An existing narrower claim under \`${body.conflict_path}\` (owned by ${areaLabel}) blocks claiming this directory. Remove the inner claim first or pick a different path.`;
        case 'equal':
          return `That exact path is already claimed by ${areaLabel} (folder \`${body.conflict_path}\`).`;
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
    'area_id' in body &&
    typeof (body as { area_id: unknown }).area_id === 'string'
  ) {
    const kind = (body as { conflict_kind: unknown }).conflict_kind;
    if (kind === 'descendant' || kind === 'ancestor' || kind === 'equal') {
      return body as FolderConflictBody;
    }
  }
  return null;
}
