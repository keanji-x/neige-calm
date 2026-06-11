import {
  lazy,
  Suspense,
  useEffect,
  useId,
  useMemo,
  useRef,
} from 'react';
import { useState } from '../shared/state';
import { Icon } from '../Icon';
import { AddPanel, type AddPanelKind } from '../shared/components/AddPanel';
import type { AddPanelMenuItem } from '../shared/components/AddPanel';
import type { Cove, Route, Wave, WaveCardSlot } from '../types';
import { Dialog } from '../ui/Dialog/Dialog';
import { SchemaForm } from '../shared/components/SchemaForm';
import { DirectoryBrowser } from '../shared/components/DirectoryPicker';
import { WaveLifecycleBadge } from '../shared/components/WaveLifecycleBadge';
import { WaveContext } from '../shared/components/WaveContext';
import { CalmApiError } from '../api/calm';
import { DeleteButton } from './_shared';
import { useOverlayState } from '../hooks/useOverlayState';
import { waveDisplayTitle } from '../shared/waveTitle';
import { OVERLAY_VIEW_MODE_SCHEMA_VERSION } from '../cards/builtins/schemaVersions';
import { excludeReportCards } from '../cards/excludeReportCards';
import { WaveReportPage } from './WaveReportPage';

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
type ViewMode = 'grid' | 'list' | 'report';
interface ViewModeOverlay {
  schemaVersion?: number;
  mode?: ViewMode;
}
const VIEW_MODE_DEFAULT: ViewModeOverlay = {};
const EMPTY_CARD_SLOTS: WaveCardSlot[] = [];

/** True when `s` is a recognized view mode. Hardens against an unknown
 *  string drifting in from a future server schema. */
function isViewMode(s: unknown): s is ViewMode {
  return s === 'grid' || s === 'list' || s === 'report';
}

const VIEW_MODE_ORDER: readonly ViewMode[] = ['grid', 'list', 'report'];
const VIEW_MODE_META = {
  grid: { label: 'Grid view', icon: 'grid' },
  list: { label: 'List view', icon: 'list' },
  report: { label: 'Report view', icon: 'report' },
} as const;

function nextViewMode(mode: ViewMode): ViewMode {
  const index = VIEW_MODE_ORDER.indexOf(mode);
  return VIEW_MODE_ORDER[(index + 1) % VIEW_MODE_ORDER.length] ?? 'grid';
}

function ViewModeCycleButton({
  value,
  onChange,
}: {
  value: ViewMode;
  onChange: (mode: ViewMode) => void;
}) {
  const next = nextViewMode(value);
  const currentMeta = VIEW_MODE_META[value];
  const nextMeta = VIEW_MODE_META[next];
  const label = `${currentMeta.label} — switch to ${nextMeta.label.toLowerCase()}`;

  return (
    <button
      type="button"
      className="view-cycle"
      aria-label={label}
      title={label}
      onClick={() => onChange(next)}
    >
      <Icon n={currentMeta.icon} s={14} sw={1.7} />
    </button>
  );
}

function formatCreateCardError(err: unknown): string {
  if (err instanceof CalmApiError) {
    const message = err.message.trim();
    if (message.length > 0) return message;
    return err.status >= 500
      ? 'Failed to create card'
      : `Request failed (${err.status})`;
  }
  if (err instanceof Error && err.message.trim().length > 0) {
    return err.message;
  }
  return 'Failed to create card';
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
  onAddCard: (waveId: string, type: AddPanelKind) => Promise<void> | void;
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
  const cards: WaveCardSlot[] = wave.cards ?? EMPTY_CARD_SLOTS;
  const displayTitle = waveDisplayTitle(wave.title);
  // Schema-driven AddPanel selections open a modal SchemaForm — kept in
  // local state, never reaches the kernel until submit.
  const [modalItem, setModalItem] = useState<AddPanelMenuItem | null>(null);
  const [modalError, setModalError] = useState<string | null>(null);

  const [directAddError, setDirectAddError] = useState<string | null>(null);

  const beginAdd = (item: AddPanelMenuItem) => {
    // Reset both error channels on every new attempt so a stale error
    // from a previous failed add doesn't linger past the next click.
    setDirectAddError(null);
    setModalError(null);
    if (!item.createSchema) {
      // No schema → immediate create (today: terminal). `onAddCard` now
      // rethrows non-contract failures (see `createFromEntry` in
      // router.tsx) so we await + catch here and surface the error
      // inline. The schema-modal branch below uses `modalError`.
      void (async () => {
        try {
          await onAddCard(wave.id, item.type);
          goGridAfterAdd();
        } catch (err) {
          setDirectAddError(formatCreateCardError(err));
        }
      })();
      return;
    }
    setModalItem(item);
  };

  const closeModal = () => {
    setModalError(null);
    setModalItem(null);
  };
  const submitModal = async (values: Record<string, string>) => {
    if (!modalItem) return;
    setModalError(null);
    try {
      await onCreateCardWithBody?.(wave.id, modalItem.type, values);
      setModalError(null);
      setModalItem(null);
      goGridAfterAdd();
    } catch (err) {
      setModalError(formatCreateCardError(err));
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

  const showPct = wave.progress > 0 && wave.progress < 1.0;

  const workerCardSlots = useMemo(() => excludeReportCards(cards), [cards]);
  const workerCards = useMemo(
    () => workerCardSlots.map((entry) => entry.slot),
    [workerCardSlots],
  );

  const [viewModeOverlay, setViewModeOverlay] = useOverlayState<ViewModeOverlay>({
    entity_kind: 'view',
    entity_id: wave.id,
    kind: 'view-mode',
    default: VIEW_MODE_DEFAULT,
  });
  const overlayMode = viewModeOverlay.mode;
  // Default to report. Backend mints a wave-report card for every wave at
  // create time (`crates/calm-server/src/wave_report.rs`
  // `WaveReportPayload::initial()` + the `idx_cards_one_report_per_wave`
  // partial unique index from migration 0013 / backfill in 0014), so this
  // default is safe. Adding a worker card auto-switches to grid (see
  // `goGridAfterAdd`) so the new card is visible immediately.
  // The header cycle button only changes this persisted overlay value.
  const viewMode: ViewMode = isViewMode(overlayMode) ? overlayMode : 'report';

  const setViewMode = (mode: ViewMode) => {
    setViewModeOverlay({
      schemaVersion: OVERLAY_VIEW_MODE_SCHEMA_VERSION,
      mode,
    });
  };

  // After a successful AddPanel-driven card create, if the user was reading
  // the report view, hand them to grid so the new worker card is visible
  // (WaveReportPage filters spec/wave-report out via excludeReportCards).
  // Error paths intentionally do NOT switch — the inline error sits in the
  // current header / modal, switching modes would hide it.
  const goGridAfterAdd = () => {
    if (viewMode === 'report') setViewMode('grid');
  };

  return (
    // Issue #229 PR B — wrap with WaveContext so the WaveReport card
    // (rendered deep inside WaveGrid/WaveList) can read the wave's
    // lifecycle for its header badge without prop-drilling. Other
    // cards ignore the context.
    <WaveContext.Provider value={{ id: wave.id, lifecycle: wave.lifecycle }}>
      <div
        className={
          'workbench' + (viewMode === 'report' ? ' workbench--report' : '')
        }
      >
        <header className="wave-header">
          <button
            className="wave-back"
            onClick={() => onGo({ name: 'cove', coveId: cove.id })}
            title={'Back to ' + cove.name}
          >
            <Icon n="back" s={14} sw={1.7} />
          </button>
          <ViewModeCycleButton value={viewMode} onChange={setViewMode} />
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
                aria-label={onRenameWave ? displayTitle : undefined}
                aria-describedby={onRenameWave ? renameHintId : undefined}
              >
                {displayTitle}
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
          {/* Issue #145 — Wave lifecycle badge. The kernel always stamps a
              lifecycle on every wave (defaults to 'draft' on create); this
              renders the current state as a small uppercase pill. After
              the WaveLifecycle unification (drop WaveStatus) this is the
              sole status-pill on the wave header — the legacy secondary
              status pill, plus the card-aggregate FSM dot/verb, used to
              re-derive the same signal and have been folded back into the
              lifecycle. The eta string survives in `wave.eta` and is
              rendered separately when set. */}
          <WaveLifecycleBadge lifecycle={wave.lifecycle} />
          {showPct && <span className="wave-percent num">{pct}%</span>}
          {directAddError && (
            <p
              className="schema-form-error wave-add-direct-error"
              role="alert"
            >
              {directAddError}
            </p>
          )}
          <span className="wave-action-cluster">
            <AddPanel onSelect={beginAdd} />
            {onDeleteWave && (
              <DeleteButton
                label={`Delete wave "${displayTitle}"`}
                confirmTitle="Delete wave?"
                confirmLabel="Delete wave"
                confirmMessage={`Delete wave "${displayTitle}"? Its cards (including any terminals) go too. This cannot be undone.`}
                onDelete={() => onDeleteWave(wave.id)}
              />
            )}
          </span>
        </span>
      </header>

      <section className="workbench-main">
        {viewMode === 'report' ? (
          <WaveReportPage wave={wave} cards={cards} />
        ) : (
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
                cards={workerCards}
                onRemoveCard={(filteredIdx) => {
                  const original = workerCardSlots[filteredIdx]?.originalIndex;
                  if (original !== undefined) onRemoveCard(wave.id, original);
                }}
              />
            ) : (
              <WaveGrid
                waveId={wave.id}
                cards={workerCards}
                onRemoveCard={(filteredIdx) => {
                  const original = workerCardSlots[filteredIdx]?.originalIndex;
                  if (original !== undefined) onRemoveCard(wave.id, original);
                }}
              />
            )}
          </Suspense>
        )}
      </section>
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
              {modalError && (
                <p className="schema-form-error schema-form-error-inset" role="alert">
                  {modalError}
                </p>
              )}
            </Dialog>
          );
        }
        return (
          <Dialog
            open
            onClose={closeModal}
            title={`New ${modalItem.label.replace(/^New\s+/i, '')}`}
          >
            {modalError && (
              <p className="schema-form-error" role="alert">
                {modalError}
              </p>
            )}
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
    </WaveContext.Provider>
  );
}
