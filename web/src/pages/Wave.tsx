import { lazy, Suspense, useEffect, useId, useRef } from 'react';
import { useState } from '../shared/state';
import { Icon } from '../Icon';
import { AddPanel, type AddPanelKind } from '../shared/components/AddPanel';
import type { AddPanelMenuItem } from '../shared/components/AddPanel';
import type { Cove, FsmState, Route, Wave, WaveCardSlot } from '../types';
import { Dialog } from '../ui/Dialog/Dialog';
import { SchemaForm } from '../shared/components/SchemaForm';
import { DirectoryBrowser } from '../shared/components/DirectoryPicker';
import { CardStatusDot } from '../shared/components/CardStatusDot';
import { DeleteButton } from './_shared';
import { useOverlayState } from '../hooks/useOverlayState';
import { OVERLAY_VIEW_MODE_SCHEMA_VERSION } from '../cards/builtins/schemaVersions';

// WaveGrid pulls in `react-grid-layout` (~50 KB minified) and is the
// heaviest single dependency on this page. Loading it lazily keeps the
// Wave route chunk small and means an empty wave (no cards yet) doesn't
// pay the grid cost at all on first paint. The flash on first navigation
// is intentional: we'd rather ship a smaller chunk than block render.
const WaveGrid = lazy(() =>
  import('../WaveGrid').then((m) => ({ default: m.WaveGrid })),
);

// WaveList is the keyboard-canonical alternative — much lighter (no RGL).
// Lazy-loaded for symmetry with WaveGrid and so that grid-mode users
// don't pay for it. The toggle below decides which to mount.
const WaveList = lazy(() =>
  import('../WaveList').then((m) => ({ default: m.WaveList })),
);

/** View-mode overlay shape — Slice 9 of issue #56. Persisted at
 *  `(plugin_id='kernel', entity_kind='view', entity_id=<waveId>, kind='view-mode')`.
 *  Kept separate from the layout overlay so users in list-only mode never
 *  have to mint a layout row just to flip the toggle. */
type ViewMode = 'grid' | 'list';
interface ViewModeOverlay {
  schemaVersion?: number;
  mode: ViewMode;
}
const VIEW_MODE_DEFAULT: ViewModeOverlay = { mode: 'grid' };

/** True when `s` is a recognized view mode. Hardens against an unknown
 *  string drifting in from a future server schema. */
function isViewMode(s: unknown): s is ViewMode {
  return s === 'grid' || s === 'list';
}

/** Short verb for each FSM state — used by the wave-header status pill. */
function fsmVerb(s: FsmState): string {
  switch (s) {
    case 'Starting':
      return 'Starting';
    case 'Working':
      return 'Working';
    case 'AwaitingInput':
      return 'Waiting on you';
    case 'Errored':
      return 'Errored';
    case 'Done':
      return 'Done';
    case 'Idle':
    default:
      return 'Idle';
  }
}

// ============================================================
// WavePage — workbench: thin header + stacked cards.
// Drag is restricted to the ⠿ grip so xterm / inputs inside cards stay usable.
// ============================================================

export function WavePage({
  wave,
  cove,
  onGo,
  onAddCard,
  onCreateCardWithBody,
  onRemoveCard,
  onRenameWave,
  onDeleteWave,
}: {
  wave: Wave;
  cove: Cove;
  onGo: (r: Route) => void;
  /** No-schema "create immediately" path — kept for terminal cards which
   *  spawn with default args. */
  onAddCard: (waveId: string, type: AddPanelKind) => void;
  /** Schema-driven path — invoked after the user submits a config card.
   *  The Wave-level dispatcher knows how to translate per-kind values
   *  into the right kernel calls. */
  onCreateCardWithBody?: (
    waveId: string,
    type: AddPanelKind,
    values: Record<string, string>,
  ) => Promise<void>;
  onRemoveCard: (waveId: string, idx: number) => void;
  onRenameWave?: (waveId: string, title: string) => void | Promise<void>;
  onDeleteWave?: (waveId: string) => void | Promise<void>;
}) {
  const pct = Math.round(wave.progress * 100);
  const cards: WaveCardSlot[] = wave.cards || [];
  // Schema-driven AddPanel selections open a modal SchemaForm — kept in
  // local state, never reaches the kernel until submit.
  const [modalItem, setModalItem] = useState<AddPanelMenuItem | null>(null);

  const beginAdd = (item: AddPanelMenuItem) => {
    if (!item.createSchema) {
      // No schema → immediate create (today: terminal).
      onAddCard(wave.id, item.type);
      return;
    }
    setModalItem(item);
  };

  const closeModal = () => setModalItem(null);
  const submitModal = async (values: Record<string, string>) => {
    if (!modalItem) return;
    try {
      await onCreateCardWithBody?.(wave.id, modalItem.type, values);
    } finally {
      setModalItem(null);
    }
  };

  // Inline rename state. The title sits inside the breadcrumb so we
  // swap a same-class input in place of the span when editing — no
  // layout shift, the rest of the header stays put.
  const [editingTitle, setEditingTitle] = useState(false);
  const [draftTitle, setDraftTitle] = useState(wave.title);
  const titleInputRef = useRef<HTMLInputElement | null>(null);
  // Keep a ref on the display span so we can return focus to it when
  // edit mode exits — both for the Escape-cancel path and the
  // Enter/blur-commit path. Without this, focus would drop to body
  // after the input unmounts and the keyboard user would lose place.
  const titleDisplayRef = useRef<HTMLSpanElement | null>(null);
  // When a commit/cancel restores focus to the display span we set
  // this flag so the effect can run once the unmount has flushed.
  const restoreTitleFocus = useRef(false);
  // Stable id for the visually-hidden rename hint. Same accessible-name
  // split as CovePage's EditableTitle (#56 followup): the title's
  // aria-label is just the wave name and the rename verb lives in a
  // sibling span referenced via aria-describedby.
  const renameHintId = useId();
  useEffect(() => {
    if (!editingTitle) {
      setDraftTitle(wave.title);
      if (restoreTitleFocus.current) {
        restoreTitleFocus.current = false;
        titleDisplayRef.current?.focus();
      }
    }
  }, [editingTitle, wave.title]);
  const startRename = () => {
    if (!onRenameWave) return;
    setDraftTitle(wave.title);
    setEditingTitle(true);
    queueMicrotask(() => {
      titleInputRef.current?.focus();
      titleInputRef.current?.select();
    });
  };
  const commitRename = async () => {
    const trimmed = draftTitle.trim();
    restoreTitleFocus.current = true;
    setEditingTitle(false);
    if (!trimmed || trimmed === wave.title || !onRenameWave) return;
    await onRenameWave(wave.id, trimmed);
  };
  const cancelRename = () => {
    restoreTitleFocus.current = true;
    setEditingTitle(false);
  };

  // Show eta pill only when there's text to show.
  const showEtaPill = !!wave.eta;
  const showPct = wave.progress > 0 && wave.progress < 1.0;

  // Per-wave view-mode preference. Defaults to grid (no breaking change
  // for mouse users); the toggle in the wave-header flips it and the
  // overlay persists across reloads. The hook handles optimistic update,
  // WS replay, and IndexedDB rehydration the same way the layout
  // overlay does.
  const [viewModeOverlay, setViewModeOverlay] = useOverlayState<ViewModeOverlay>({
    entity_kind: 'view',
    entity_id: wave.id,
    kind: 'view-mode',
    default: VIEW_MODE_DEFAULT,
  });
  const viewMode: ViewMode = isViewMode(viewModeOverlay.mode)
    ? viewModeOverlay.mode
    : 'grid';
  const toggleViewMode = () => {
    setViewModeOverlay({
      schemaVersion: OVERLAY_VIEW_MODE_SCHEMA_VERSION,
      mode: viewMode === 'grid' ? 'list' : 'grid',
    });
  };

  return (
    <div className="workbench">
      <header className="wave-header">
        <button
          className="wave-back"
          onClick={() => onGo({ name: 'cove', coveId: cove.id })}
          title={'Back to ' + cove.name}
        >
          <Icon n="back" s={14} sw={1.7} />
        </button>
        <span className="wave-crumb">
          <span className="wave-cove-dot" style={{ background: cove.color }} />
          <button
            type="button"
            className="wave-cove"
            onClick={() => onGo({ name: 'cove', coveId: cove.id })}
          >
            {cove.name}
          </button>
          <span className="wave-sep">·</span>
          {editingTitle ? (
            <input
              ref={titleInputRef}
              className="wave-title wave-title-input"
              value={draftTitle}
              onChange={(e) => setDraftTitle(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') void commitRename();
                else if (e.key === 'Escape') cancelRename();
              }}
              onBlur={() => void commitRename()}
              aria-label="Wave title"
            />
          ) : (
            // Keyboard entry: the span is tab-stop + role=button so Enter/F2
            // open rename mode without needing a pointer. The visual styling
            // is unchanged (cursor: text); only the focus-visible ring shows
            // to keyboard users. See calm.css `.wave-title[role="button"]`.
            //
            // Accessible-name split (#56 followup): aria-label is just the
            // wave title; the rename verb lives in a sibling sr-only span
            // referenced via aria-describedby. Keeps the breadcrumb's
            // accessible name uncluttered (parity with CovePage's heading).
            <>
              <span
                ref={titleDisplayRef}
                className="wave-title"
                onClick={onRenameWave ? startRename : undefined}
                onKeyDown={
                  onRenameWave
                    ? (e) => {
                        if (e.key === 'Enter' || e.key === 'F2') {
                          e.preventDefault();
                          startRename();
                        }
                      }
                    : undefined
                }
                style={onRenameWave ? { cursor: 'text' } : undefined}
                title={onRenameWave ? 'Click to rename' : undefined}
                role={onRenameWave ? 'button' : undefined}
                tabIndex={onRenameWave ? 0 : undefined}
                aria-label={onRenameWave ? wave.title : undefined}
                aria-describedby={onRenameWave ? renameHintId : undefined}
              >
                {wave.title}
              </span>
              {onRenameWave && (
                <span id={renameHintId} className="sr-only">
                  Rename wave
                </span>
              )}
            </>
          )}
        </span>
        <span className="wave-meta">
          {/* View-mode toggle (Slice 9 of issue #56). Two-state button —
              `role="switch"` + `aria-checked` so AT announces the bound
              state ("Grid view, switch, on" vs "List view, switch, off").
              The accessible name carries the layout vocabulary so a
              keyboard / screen-reader user knows what the control
              switches between; the visible label flips with the state.
              Pressed visually matches AddPanel — dashed border swap. */}
          <button
            type="button"
            role="switch"
            aria-checked={viewMode === 'list'}
            className={'view-toggle' + (viewMode === 'list' ? ' is-list' : '')}
            onClick={toggleViewMode}
            title={
              viewMode === 'list'
                ? 'Switch to grid view'
                : 'Switch to list view (keyboard-friendly)'
            }
            aria-label={
              viewMode === 'list'
                ? 'Switch wave to grid view'
                : 'Switch wave to list view'
            }
          >
            {viewMode === 'list' ? 'List' : 'Grid'}
          </button>
          <AddPanel onSelect={beginAdd} />
          {/* 6-state FSM dot + verb. Rendered whenever the kernel `card_fsm`
              has assigned a state to this wave (only happens when at least
              one tracked card — today: codex — exists in the wave). Falls
              through to the legacy 3-state status pill when no FSM state is
              present (older overlays, terminal-only waves). */}
          {wave.fsmState ? (
            <span className="status-pill" title={fsmVerb(wave.fsmState)}>
              <CardStatusDot state={wave.fsmState} />
              <span style={{ marginLeft: 6 }}>
                {fsmVerb(wave.fsmState)}
                {wave.counts && wave.counts.working > 1
                  ? ` (${wave.counts.working})`
                  : ''}
              </span>
            </span>
          ) : (
            <>
              {wave.status === 'running' && (
                <span className="status-pill running">
                  <span className="status-pill-dot live-dot" />
                  {wave.eta || 'running'}
                </span>
              )}
              {wave.status === 'waiting' && showEtaPill && (
                <span className="status-pill waiting">
                  <span className="status-pill-dot warn" />
                  {wave.eta}
                </span>
              )}
            </>
          )}
          {showPct && <span className="wave-percent num">{pct}%</span>}
          {onDeleteWave && (
            <DeleteButton
              label={`Delete wave "${wave.title}"`}
              confirmTitle="Delete wave?"
              confirmLabel="Delete wave"
              confirmMessage={`Delete wave "${wave.title}"? Its cards (including any terminals) go too. This cannot be undone.`}
              onDelete={() => onDeleteWave(wave.id)}
            />
          )}
        </span>
      </header>

      <main className="workbench-main">
        <Suspense
          fallback={
            <div className="synth">
              {viewMode === 'list' ? 'Loading list…' : 'Loading grid…'}
            </div>
          }
        >
          {viewMode === 'list' ? (
            <WaveList
              waveId={wave.id}
              cards={cards}
              onRemoveCard={(idx) => onRemoveCard(wave.id, idx)}
            />
          ) : (
            <WaveGrid
              waveId={wave.id}
              cards={cards}
              onRemoveCard={(idx) => onRemoveCard(wave.id, idx)}
            />
          )}
        </Suspense>
      </main>
      {/* Shortcut: when a kind's createSchema is just one `directory` field,
          skip the SchemaForm wrapper entirely and let the user pick a
          directory = create. Today only codex hits this path. Other kinds
          (multi-field schemas) still go through the SchemaForm. */}
      {(() => {
        if (!modalItem) return null;
        const fields = modalItem.createSchema?.fields ?? [];
        const soleDir =
          fields.length === 1 && fields[0].type === 'directory' ? fields[0] : null;
        if (soleDir) {
          return (
            <Dialog
              open
              onClose={closeModal}
              title={`New ${modalItem.label.replace(/^New\s+/i, '')}`}
              wide
            >
              <DirectoryBrowser
                initialPath={null}
                onCancel={closeModal}
                onSelect={(path) => submitModal({ [soleDir.key]: path })}
                selectLabel="Create here"
              />
            </Dialog>
          );
        }
        return (
          <Dialog
            open
            onClose={closeModal}
            title={`New ${modalItem.label.replace(/^New\s+/i, '')}`}
          >
            {modalItem.createSchema && (
              <SchemaForm
                schema={modalItem.createSchema}
                submitLabel="Create"
                onSubmit={submitModal}
                onCancel={closeModal}
              />
            )}
          </Dialog>
        );
      })()}
    </div>
  );
}
