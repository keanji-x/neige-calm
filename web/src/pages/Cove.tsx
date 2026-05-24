import { useEffect, useId, useMemo, useRef } from 'react';
import { useState } from '../shared/state';
import { WaveRow } from '../shared/components/WaveRow';
import { NewTaskForm } from '../shared/components/NewTaskForm';
import type { NewTaskFormResult } from '../shared/components/NewTaskForm';
import { isRunning, isWaitingForUser } from '../shared/lifecycle';
import type { Cove, Route, Wave } from '../types';
import { ConfirmDialog } from '../ui/ConfirmDialog/ConfirmDialog';
import { Dialog } from '../ui/Dialog/Dialog';
import { DeleteButton } from './_shared';

// ============================================================
// CovePage — a single sorted list of the cove's waves.
//
// Each row encodes its own status (glyph/dot + activity + progress) so we
// don't need Waiting / Running / Idle section headings to convey it.
// Rows are ordered `waiting → running → idle` with input order preserved
// within each bucket; that puts whatever needs attention at the top
// without forcing the user to read three section labels to know it.
//
// The cove name renders exactly once — as the page <h1> — so there's no
// duplicate naming chain with the sidebar's cove-nav row (which already
// shows the cove name + swatch + wave count) or with the page header.
// No eyebrow row above the title: the sidebar already carries the cove's
// color swatch + wave count, so reprinting either here is redundant
// noise. "Less is more" — the page opens directly on the title.
// ============================================================

export function CovePage({
  cove,
  waves,
  onGo,
  onWaveCreated,
  onRenameCove,
  onDeleteCove,
  onDeleteWave,
}: {
  cove: Cove;
  waves: Wave[];
  onGo: (r: Route) => void;
  /** Issue #250 PR 3 — fired after NewTaskForm successfully POSTs
   *  `/api/waves`. Caller navigates to the new wave's detail page.
   *  CovePage owns the inline-form open/close state; the create POST
   *  + theme stamping + cove auto-inference all live inside
   *  NewTaskForm. Pre-PR 3 this prop was `(coveId, title)` and the
   *  caller did the create — that signature is gone. */
  onWaveCreated?: (wave: NewTaskFormResult) => void | Promise<void>;
  /** Called from the inline rename input on the header. */
  onRenameCove?: (coveId: string, name: string) => void | Promise<void>;
  /** Called from the × button on the header. CovePage renders a
   *  ConfirmDialog inside `DeleteButton`, so callers don't need to
   *  double-prompt. */
  onDeleteCove?: (coveId: string) => void | Promise<void>;
  /** Called from a per-row × on hover. CovePage renders a single
   *  ConfirmDialog at the page level (driven by `pendingDeleteWave`)
   *  and routes confirmed deletes to this callback. */
  onDeleteWave?: (waveId: string) => void | Promise<void>;
}) {
  // Per-row delete uses Pattern B (close-then-await): the dialog closes
  // on Confirm and the parent's onDeleteWave promise runs without UI
  // gating. The wave row already vanishes from the list on completion
  // (the parent removes it from `waves`), which acts as its own
  // "succeeded" signal — gating the dialog with `confirmDisabled` would
  // duplicate that affordance. Cancel-safe default focus is inherited
  // from ConfirmDialog.
  const [pendingDeleteWave, setPendingDeleteWave] = useState<Wave | null>(null);
  const openDeleteWaveDialog = (w: Wave) => {
    if (!onDeleteWave) return;
    setPendingDeleteWave(w);
  };
  const cancelDeleteWave = () => setPendingDeleteWave(null);
  const confirmDeleteWave = () => {
    const w = pendingDeleteWave;
    setPendingDeleteWave(null);
    if (!w || !onDeleteWave) return;
    void onDeleteWave(w.id);
  };

  // Single sorted list: waiting first (needs the user), then running
  // (in-flight work), then other (draft/done/canceled — the quiet
  // default). Within each bucket we keep the caller's order — the
  // parent already orders waves the way the user expects (by recency /
  // sort field). A stable bucket sort expresses status without forking
  // the layout into separate sections. Bucket rank is derived from the
  // wave's `WaveLifecycle` (the single source of truth for wave-level
  // state — see `shared/lifecycle.ts`).
  const sortedWaves = useMemo(() => {
    const rankOf = (w: Wave): number => {
      if (isWaitingForUser(w.lifecycle)) return 0;
      if (isRunning(w.lifecycle)) return 1;
      return 2;
    };
    return [...waves].sort((a, b) => rankOf(a) - rankOf(b));
  }, [waves]);

  return (
    <div className="col wide">
      {/* `.cove-head` is a flex row so the × centers naturally against
          the h1's line-box; the × is pushed to the right edge via
          `margin-left: auto`. The page title's leading edge stays at
          x=0 (matching every wave-row's glyph column below). The × is
          hover/focus-within revealed, mirroring the per-row × on each
          WaveRow so the page has one consistent delete affordance. */}
      <header className="cove-head">
        {onRenameCove ? (
          <EditableTitle
            value={cove.name}
            ariaLabel="Cove name"
            onSave={(name) => onRenameCove(cove.id, name)}
          />
        ) : (
          <h1 className="h-display">{cove.name}.</h1>
        )}
        {onDeleteCove && (
          <span className="cove-head-delete">
            <DeleteButton
              label={`Delete cove "${cove.name}"`}
              confirmTitle="Delete cove?"
              confirmLabel="Delete cove"
              confirmMessage={`Delete cove "${cove.name}"? Its waves and cards go too. This cannot be undone.`}
              onDelete={() => onDeleteCove(cove.id)}
            />
          </span>
        )}
      </header>

      {waves.length === 0 && (
        <div
          style={{
            padding: '32px 0 8px', color: 'var(--text-3)',
            fontSize: 15, textAlign: 'center',
          }}
        >
          This Cove is quiet. Start a Wave below.
        </div>
      )}

      {/* The wave list lives inside a single `aria-label`-ed region so
          role-scoped locators (axe scans + Playwright `getByRole('region',
          { name: 'Waves' })`) can disambiguate WaveRow buttons from the
          sidebar's "Today" nav button. The landmark gives tests a clean
          scope. See §2.2 of `docs/a11y-contract.md`. */}
      {sortedWaves.length > 0 && (
        <section aria-label="Waves">
          <div className="waves">
            {sortedWaves.map((w) => (
              <WaveRow
                key={w.id}
                wave={w}
                cove={cove}
                showCove={false}
                onClick={() => onGo({ name: 'wave', id: w.id })}
                onDelete={onDeleteWave ? () => openDeleteWaveDialog(w) : undefined}
              />
            ))}
          </div>
        </section>
      )}

      {onWaveCreated && (
        <NewWaveCTA defaultCoveId={cove.id} onCreated={onWaveCreated} />
      )}

      {/* Page-level wave-delete confirmation. One dialog instance per
          page; `pendingDeleteWave` carries the wave being confirmed so
          the title row text reflects the actual wave name. Mounted
          unconditionally with `open` driven by the pending state so
          Dialog's focus restore on close lands us back where we were. */}
      <ConfirmDialog
        open={pendingDeleteWave !== null}
        title="Delete wave?"
        description={
          pendingDeleteWave
            ? `Delete wave "${pendingDeleteWave.title}"? Its cards (including any terminals) go too. This cannot be undone.`
            : null
        }
        confirmLabel="Delete wave"
        cancelLabel="Cancel"
        onConfirm={confirmDeleteWave}
        onCancel={cancelDeleteWave}
      />
    </div>
  );
}

/**
 * Title with an inline-edit affordance.
 *
 * The pencil button switches the h1 to a same-sized input. Enter / blur
 * save (no-op if unchanged or empty); Escape cancels. The input inherits
 * the h1's visual styling so editing feels like the title sliding open,
 * not a popover. The trailing period in the design (`cove.name + '.'`)
 * is rendered by the parent, not stored — the editor edits the raw name.
 */
function EditableTitle({
  value,
  onSave,
  ariaLabel,
}: {
  value: string;
  onSave: (next: string) => void | Promise<void>;
  ariaLabel: string;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  const inputRef = useRef<HTMLInputElement | null>(null);
  // Display-mode button ref so we can return focus to it when edit mode
  // exits (Escape-cancel or Enter/blur-commit). Without this, focus
  // drops to body and the keyboard user loses their place.
  const displayRef = useRef<HTMLButtonElement | null>(null);
  const restoreDisplayFocus = useRef(false);
  // Re-entry guard for issue #288.
  //
  // When the user commits a rename via Enter, this sequence happens:
  //   1. Enter `keydown` fires on the input → `save()` → `setEditing(false)`
  //      → PATCH fires with the new name.
  //   2. React commits — input unmounts, display button mounts, focus
  //      restores to the display button via `restoreDisplayFocus`.
  //   3. Browser delivers Enter's `keyup` to the now-focused display
  //      button. The browser then synthesizes a `click` from that keyup
  //      (intrinsic <button> activation semantics).
  //   4. The synthetic click fires `enter()` → `setDraft(value)` →
  //      `setEditing(true)`. `value` at this moment is still the OLD
  //      name (the optimistic cache update has flushed to props but the
  //      WS round-trip may also race in here), so `draft` is reset to
  //      the OLD name.
  //   5. The user's follow-up Tab / click-away triggers `onBlur` →
  //      `save()` → trimmed = OLD, value = NEW (from optimistic) →
  //      `trimmed !== value` so the early-return doesn't catch it →
  //      **a second PATCH fires that writes the OLD name back to the
  //      kernel.** The sidebar then flashes the new name (from the
  //      optimistic update / first WS event) and reverts to the old
  //      name when the second PATCH's WS event arrives.
  //
  // The fix: when `save()` is invoked from a keyboard commit, set this
  // ref. `enter()` consumes & clears it on the next click, suppressing
  // the synthetic Enter-keyup click. Real mouse clicks always have
  // `detail >= 1`, but we don't gate on that — we just consume the
  // one-shot flag, so a legitimate click that races in right after a
  // commit will be eaten and the user has to click again. That cost is
  // acceptable: it's a single click within a ~tick window of a commit,
  // and the alternative (the kernel ending up with the wrong name) is
  // worse.
  const suppressNextDisplayActivation = useRef(false);
  // Stable id for the visually-hidden rename hint. The hint is referenced
  // via aria-describedby so the button's accessible *name* is just the
  // cove name (clean heading-nav narration) while AT still verbalizes the
  // rename verb as a *description*. Generated via useId so multiple
  // EditableTitles on the same page would not collide.
  const hintId = useId();

  // External value changes (e.g. WS event from another tab) should not
  // clobber an in-flight edit; only sync `draft` when not editing.
  useEffect(() => {
    if (!editing) {
      setDraft(value);
      if (restoreDisplayFocus.current) {
        restoreDisplayFocus.current = false;
        displayRef.current?.focus();
      }
    }
  }, [editing, value]);

  const enter = () => {
    // Eat the synthetic click that follows a keyboard commit (see the
    // `suppressNextDisplayActivation` doc above). The flag was set by
    // `save()` when invoked from the Enter keydown handler.
    if (suppressNextDisplayActivation.current) {
      suppressNextDisplayActivation.current = false;
      return;
    }
    setDraft(value);
    setEditing(true);
    queueMicrotask(() => {
      inputRef.current?.focus();
      inputRef.current?.select();
    });
  };
  const cancel = () => {
    restoreDisplayFocus.current = true;
    setEditing(false);
  };
  const save = async (opts: { viaKeyboard?: boolean } = {}) => {
    const trimmed = draft.trim();
    restoreDisplayFocus.current = true;
    // Issue #288 — when this save is the keyboard-Enter commit, arm the
    // one-shot click suppressor so the Enter `keyup` that the browser
    // delivers to the newly-focused display button doesn't synthesize a
    // click that re-enters edit mode with a stale draft. See the
    // `suppressNextDisplayActivation` ref above for the full sequence.
    if (opts.viaKeyboard) {
      suppressNextDisplayActivation.current = true;
    }
    setEditing(false);
    if (!trimmed || trimmed === value) return;
    await onSave(trimmed);
  };

  if (editing) {
    // The input rides `.h-display`'s typography and `.cove-title-input`'s
    // block-level width + focus-ring CSS (calm.css). The default
    // `.h-display { margin: 0 0 var(--space-10) }` carries the same
    // bottom-rhythm as the display-mode <h1>, so swapping between modes
    // doesn't shift the row below. No inline styles — keep the rename
    // input's chrome owned by CSS so it stays in lockstep with the page
    // title's typography.
    return (
      <input
        ref={inputRef}
        value={draft}
        onChange={(e) => setDraft(e.target.value)}
        onKeyDown={(e) => {
          if (e.key === 'Enter') void save({ viaKeyboard: true });
          else if (e.key === 'Escape') cancel();
        }}
        onBlur={() => void save()}
        aria-label={ariaLabel}
        className="h-display cove-title-input"
      />
    );
  }
  // Click-to-edit: no pencil affordance — the title itself is the
  // affordance. `cursor: text` is the hint; click → enter edit mode.
  //
  // Keyboard entry: rendered as a real <button> so the role is intrinsic
  // (Enter/Space activate it for free) and screen readers announce it as
  // an actionable control. We add F2 explicitly for the Windows rename
  // convention, and call `preventDefault` on both to suppress the native
  // button click that Space/Enter would synthesize. The <h1> wraps the
  // button so heading-level navigation still lands on "the title" without
  // sacrificing the interactive semantics.
  //
  // Accessible-name split (#56 followup): the button's aria-label is just
  // the cove name so heading-nav (`H` in screen readers) announces e.g.
  // "Atlas, heading level 1" — not "Rename cove name: Atlas, heading
  // level 1". The rename verb lives in a visually-hidden sibling span
  // *outside* the <h1> (referenced via aria-describedby), so the heading's
  // own accessible-name computation isn't polluted by the helper text.
  // AT still verbalizes "Rename cove name" as the button's *description*
  // when the button is focused.
  return (
    <>
      <h1 className="h-display">
        <button
          ref={displayRef}
          type="button"
          className="h-display-rename"
          onClick={enter}
          onKeyDown={(e) => {
            if (e.key === 'F2') {
              e.preventDefault();
              enter();
            }
          }}
          title="Click to rename"
          aria-label={value}
          aria-describedby={hintId}
        >
          {value}.
        </button>
      </h1>
      <span id={hintId} className="sr-only">
        {`Rename ${ariaLabel.toLowerCase()}`}
      </span>
    </>
  );
}

// ---------------- NewWaveCTA — CovePage's compose-bar ----------------
//
// Bottom-of-page ghost button. Clicking opens a modal Dialog containing
// the shared `NewTaskForm` configuration card. Issue #250 PR 3 — per the
// issue comment "all creation entrypoints must go through the same
// configuration card", this CTA opens NewTaskForm rather than a bespoke
// one-line title input. The calendar's empty-cell click (PR 6) opens
// the same NewTaskForm via the same component.
//
// Why a Dialog (vs. the original inline expansion): the cwd field
// benefits from a Browse… affordance that takes over the whole modal
// body via `useModalView()` (the same pattern the codex card uses).
// That hook is a no-op outside a Dialog, so wrapping the form in a
// Dialog is the prerequisite. The button stays visible while the
// dialog is open so Dialog's focus-restore returns the keyboard user
// to where they started on close.

function NewWaveCTA({
  defaultCoveId,
  onCreated,
}: {
  defaultCoveId: string;
  onCreated: (wave: NewTaskFormResult) => void | Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const close = () => setOpen(false);
  // Shared ref between the host Dialog and the NewTaskForm title
  // textarea so the Dialog's initial-focus pass lands directly on the
  // description field. Without this, NewTaskForm's mount-time
  // queueMicrotask(focus) would race against Dialog's rAF "focus first
  // focusable" — and the rAF, scheduled later, would win and land focus
  // on the Dialog's Close button. Forwarding the ref makes the Dialog
  // do the focusing once, deterministically.
  const titleRef = useRef<HTMLTextAreaElement | null>(null);
  return (
    <>
      <button
        type="button"
        className="new-wave-cta"
        onClick={() => setOpen(true)}
        title="New wave"
      >
        <span className="new-wave-glyph" aria-hidden>+</span>
        <span className="new-wave-label">New wave</span>
      </button>
      <Dialog
        open={open}
        onClose={close}
        title="New wave"
        initialFocusRef={titleRef}
      >
        {open && (
          <NewTaskForm
            defaultCoveId={defaultCoveId}
            onCreated={async (wave) => {
              close();
              await onCreated(wave);
            }}
            onCancel={close}
            initialFocusRef={titleRef}
          />
        )}
      </Dialog>
    </>
  );
}
