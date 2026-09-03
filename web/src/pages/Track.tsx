import {
  lazy,
  Suspense,
  useEffect,
  useId,
  useMemo,
  useRef,
} from 'react';
import { useRouterState } from '@tanstack/react-router';
import { useState } from '../shared/state';
import { Icon } from '../Icon';
import { AddPanel, type AddPanelKind } from '../shared/components/AddPanel';
import type { AddPanelMenuItem } from '../shared/components/AddPanel';
import type { Area, Route, Track, TrackCardSlot } from '../types';
import { Dialog } from '../ui/Dialog/Dialog';
import { SchemaForm } from '../shared/components/SchemaForm';
import { DirectoryBrowser } from '../shared/components/DirectoryPicker';
import { TrackLifecycleBadge } from '../shared/components/TrackLifecycleBadge';
import { TrackContext } from '../shared/components/TrackContext';
import { CalmApiError } from '../api/calm';
import { DeleteButton } from './_shared';
import { useOverlayState } from '../hooks/useOverlayState';
import { trackDisplayTitle } from '../shared/trackTitle';
import { OVERLAY_VIEW_MODE_SCHEMA_VERSION } from '../cards/builtins/schemaVersions';
import { excludeReportCards } from '../cards/excludeReportCards';
import { decodeHash, TrackReportPage } from './TrackReportPage';

// TrackGrid pulls in `react-grid-layout` (~50 KB minified) and is the
// heaviest single dependency on this page. Loading it lazily keeps the
// Track route chunk small and means an empty track (no cards yet) doesn't
// pay the grid cost at all on first paint. The flash on first navigation
// is intentional: we'd rather ship a smaller chunk than block render.
const TrackGrid = lazy(() =>
  import('../TrackGrid').then((m) => ({ default: m.TrackGrid })),
);

// TrackList is the keyboard-canonical alternative — much lighter (no RGL).
// Lazy-loaded for symmetry with TrackGrid and so that grid-mode users
// don't pay for it. The toggle below decides which to mount.
const TrackList = lazy(() =>
  import('../TrackList').then((m) => ({ default: m.TrackList })),
);

/** View-mode overlay shape — Slice 9 of issue #56. Persisted at
 *  `(plugin_id='kernel', entity_kind='view', entity_id=<trackId>, kind='view-mode')`.
 *  Kept separate from the layout overlay so users in list-only mode never
 *  have to mint a layout row just to flip the toggle. */
type ViewMode = 'grid' | 'list' | 'report';
interface ViewModeOverlay {
  schemaVersion?: number;
  mode?: ViewMode;
}
const VIEW_MODE_DEFAULT: ViewModeOverlay = {};
const EMPTY_CARD_SLOTS: TrackCardSlot[] = [];

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
// TrackPage — workbench: thin header + stacked cards.
// Drag is restricted to the ⠿ grip so xterm / inputs inside cards stay usable.
// ============================================================

export function TrackPage({
  track,
  area,
  onGo,
  onAddCard,
  onCreateCardWithBody,
  onRemoveCard,
  onRenameTrack,
  onDeleteTrack,
}: {
  track: Track;
  area: Area;
  onGo: (r: Route) => void;
  /** No-schema "create immediately" path — kept for terminal cards which
   *  spawn with default args. */
  onAddCard: (trackId: string, type: AddPanelKind) => Promise<void> | void;
  /** Schema-driven path — invoked after the user submits a config card.
   *  The Track-level dispatcher knows how to translate per-kind values
   *  into the right kernel calls. */
  onCreateCardWithBody?: (
    trackId: string,
    type: AddPanelKind,
    values: Record<string, string>,
  ) => Promise<void>;
  onRemoveCard: (trackId: string, idx: number) => void;
  onRenameTrack?: (trackId: string, title: string) => void | Promise<void>;
  onDeleteTrack?: (trackId: string) => void | Promise<void>;
}) {
  const pct = Math.round(track.progress * 100);
  const cards: TrackCardSlot[] = track.cards ?? EMPTY_CARD_SLOTS;
  const displayTitle = trackDisplayTitle(track.title);
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
          await onAddCard(track.id, item.type);
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
      await onCreateCardWithBody?.(track.id, modalItem.type, values);
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
  const [draftTitle, setDraftTitle] = useState(track.title);
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
  // split as AreaPage's EditableTitle (#56 followup): the title's
  // aria-label is just the track name and the rename verb lives in a
  // sibling span referenced via aria-describedby.
  const renameHintId = useId();
  useEffect(() => {
    if (!editingTitle) {
      setDraftTitle(track.title);
      if (restoreTitleFocus.current) {
        restoreTitleFocus.current = false;
        titleDisplayRef.current?.focus();
      }
    }
  }, [editingTitle, track.title]);
  const startRename = () => {
    if (!onRenameTrack) return;
    setDraftTitle(track.title);
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
    if (!trimmed || trimmed === track.title || !onRenameTrack) return;
    await onRenameTrack(track.id, trimmed);
  };
  const cancelRename = () => {
    restoreTitleFocus.current = true;
    setEditingTitle(false);
  };

  const showPct = track.progress > 0 && track.progress < 1.0;

  const workerCardSlots = useMemo(() => excludeReportCards(cards), [cards]);
  const workerCards = useMemo(
    () => workerCardSlots.map((entry) => entry.slot),
    [workerCardSlots],
  );

  const [viewModeOverlay, setViewModeOverlay] = useOverlayState<ViewModeOverlay>({
    entity_kind: 'view',
    entity_id: track.id,
    kind: 'view-mode',
    default: VIEW_MODE_DEFAULT,
  });
  const overlayMode = viewModeOverlay.mode;
  // Default to report. Backend mints a track-report card for every track at
  // create time (`crates/calm-server/src/track_report.rs`
  // `TrackReportPayload::initial()` + the `idx_cards_one_report_per_track`
  // partial unique index from migration 0013 / backfill in 0014), so this
  // default is safe. Adding a worker card auto-switches to grid (see
  // `goGridAfterAdd`) so the new card is visible immediately.
  // The header cycle button only changes this persisted overlay value.
  const persistedViewMode: ViewMode = isViewMode(overlayMode) ? overlayMode : 'report';

  const setViewMode = (mode: ViewMode) => {
    setViewModeOverlay({
      schemaVersion: OVERLAY_VIEW_MODE_SCHEMA_VERSION,
      mode,
    });
  };

  // After a successful AddPanel-driven card create, if the user was reading
  // the report view, hand them to grid so the new worker card is visible
  // (TrackReportPage filters spec/track-report out via excludeReportCards).
  // Error paths intentionally do NOT switch — the inline error sits in the
  // current header / modal, switching modes would hide it.
  const goGridAfterAdd = () => {
    if (viewMode === 'report') setViewMode('grid');
  };

  // A report task block's "Open worker output" links to
  // `/track/$trackId#<workerCardId>` (`pages/report-blocks/task.tsx`). The hash
  // alone used to change nothing: report mode hands every hash to
  // `revealReportBlock`, which only resolves report *block* ids, so a worker
  // card id matched nothing and the click was silently inert.
  //
  // Only a hash naming a worker card of THIS track counts. The predicate reads
  // the card list rather than matching the id shape, so a report block anchor
  // (`b_xxxx`) can never satisfy it and in-document links keep working.
  const hash = useRouterState({ select: (state) => state.location.hash });
  const revealCardId = useMemo(() => {
    const id = decodeHash(hash);
    if (!id) return undefined;
    return workerCards.some(
      (slot) => slot.kind === 'card' && slot.card?.id === id,
    )
      ? id
      : undefined;
  }, [hash, workerCards]);

  // Arriving on a card is a *navigation*, not a new preference, so it derives
  // the rendered mode instead of writing the overlay.
  //
  // Writing it was the bug: an effect that flipped `report → grid` whenever the
  // hash named a card re-fired every time the mode changed back, and the hash
  // outlives the click. Cycling round to report wrote `report`, the effect
  // wrote `grid` on top, and the report view became unreachable for the rest of
  // the visit — two persisted round-trips per attempt. Deriving cannot loop:
  // there is nothing to write back.
  //
  // Only `report` is overridden. List shows the same `data-card-id` tiles the
  // grid does and reveals in place, so a list user is never dragged out of the
  // view they chose.
  const [revealConsumed, setRevealConsumed] = useState<string | undefined>(undefined);
  const pendingReveal = revealCardId !== undefined && revealCardId !== revealConsumed;
  const viewMode: ViewMode = pendingReveal && persistedViewMode === 'report'
    ? 'grid'
    : persistedViewMode;

  // A changed anchor is a new arrival, so it re-arms. Without this, going
  // `#card-a` → anywhere else → `#card-a` again would find `card-a` still
  // marked consumed and silently do nothing — the very symptom this page is
  // being fixed for, one navigation further along.
  const lastRevealRef = useRef(revealCardId);
  if (lastRevealRef.current !== revealCardId) {
    lastRevealRef.current = revealCardId;
    if (revealConsumed !== undefined) setRevealConsumed(undefined);
  }

  // Any explicit view choice consumes the hash: the user has now said what they
  // want to look at, and a stale anchor must not keep overriding them.
  //
  // It also *drops* the anchor from the URL, which is what makes a second click
  // on the same link work. Consumption alone cannot: clicking a `<Link>` whose
  // hash already matches the location is not a navigation, so nothing would
  // change and the reveal would never re-arm. `onGo` re-navigates to this same
  // track without a hash, so the next click is a real change again.
  const chooseViewMode = (mode: ViewMode) => {
    setRevealConsumed(revealCardId);
    if (revealCardId !== undefined) onGo({ name: 'track', id: track.id });
    setViewMode(mode);
  };

  return (
    // Issue #229 PR B — wrap with TrackContext so the TrackReport card
    // (rendered deep inside TrackGrid/TrackList) can read the track's
    // lifecycle for its header badge without prop-drilling. Other
    // cards ignore the context.
    <TrackContext.Provider value={{ id: track.id, lifecycle: track.lifecycle }}>
      <div
        className={
          'workbench' + (viewMode === 'report' ? ' workbench--report' : '')
        }
      >
        <header className="track-header">
          <button
            className="track-back"
            onClick={() => onGo({ name: 'area', areaId: area.id })}
            title={'Back to ' + area.name}
          >
            <Icon n="back" s={14} sw={1.7} />
          </button>
          <ViewModeCycleButton value={viewMode} onChange={chooseViewMode} />
          <span className="track-crumb">
          <span className="track-area-dot" style={{ background: area.color }} />
          <button
            type="button"
            className="track-area"
            onClick={() => onGo({ name: 'area', areaId: area.id })}
          >
            {area.name}
          </button>
          <span className="track-sep">·</span>
          {editingTitle ? (
            <input
              ref={titleInputRef}
              className="track-title track-title-input"
              value={draftTitle}
              onChange={(e) => setDraftTitle(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') void commitRename();
                else if (e.key === 'Escape') cancelRename();
              }}
              onBlur={() => void commitRename()}
              aria-label="Track title"
            />
          ) : (
            // Keyboard entry: the span is tab-stop + role=button so Enter/F2
            // open rename mode without needing a pointer. The visual styling
            // is unchanged (cursor: text); only the focus-visible ring shows
            // to keyboard users. See calm.css `.track-title[role="button"]`.
            //
            // Accessible-name split (#56 followup): aria-label is just the
            // track title; the rename verb lives in a sibling sr-only span
            // referenced via aria-describedby. Keeps the breadcrumb's
            // accessible name uncluttered (parity with AreaPage's heading).
            <>
              <span
                ref={titleDisplayRef}
                className="track-title"
                onClick={onRenameTrack ? startRename : undefined}
                onKeyDown={
                  onRenameTrack
                    ? (e) => {
                        if (e.key === 'Enter' || e.key === 'F2') {
                          e.preventDefault();
                          startRename();
                        }
                      }
                    : undefined
                }
                style={onRenameTrack ? { cursor: 'text' } : undefined}
                title={onRenameTrack ? 'Click to rename' : undefined}
                role={onRenameTrack ? 'button' : undefined}
                tabIndex={onRenameTrack ? 0 : undefined}
                aria-label={onRenameTrack ? displayTitle : undefined}
                aria-describedby={onRenameTrack ? renameHintId : undefined}
              >
                {displayTitle}
              </span>
              {onRenameTrack && (
                <span id={renameHintId} className="sr-only">
                  Rename track
                </span>
              )}
            </>
          )}
        </span>
        <span className="track-meta">
          {/* Issue #145 — Track lifecycle badge. The kernel always stamps a
              lifecycle on every track (defaults to 'draft' on create); this
              renders the current state as a small uppercase pill. After
              the TrackLifecycle unification (drop TrackStatus) this is the
              sole status-pill on the track header — the legacy secondary
              status pill, plus the card-aggregate FSM dot/verb, used to
              re-derive the same signal and have been folded back into the
              lifecycle. The eta string survives in `track.eta` and is
              rendered separately when set. */}
          <TrackLifecycleBadge lifecycle={track.lifecycle} />
          {showPct && <span className="track-percent num">{pct}%</span>}
          {directAddError && (
            <p
              className="schema-form-error track-add-direct-error"
              role="alert"
            >
              {directAddError}
            </p>
          )}
          <span className="track-action-cluster">
            <AddPanel onSelect={beginAdd} />
            {onDeleteTrack && (
              <DeleteButton
                label={`Delete track "${displayTitle}"`}
                confirmTitle="Delete track?"
                confirmLabel="Delete track"
                confirmMessage={`Delete track "${displayTitle}"? Its cards (including any terminals) go too. This cannot be undone.`}
                onDelete={() => onDeleteTrack(track.id)}
              />
            )}
          </span>
        </span>
      </header>

      <section className="workbench-main">
        {viewMode === 'report' ? (
          <TrackReportPage track={track} cards={cards} />
        ) : (
          <Suspense
            fallback={
              <div className="synth">
                {viewMode === 'list' ? 'Loading list…' : 'Loading grid…'}
              </div>
            }
          >
            {viewMode === 'list' ? (
              <TrackList
                trackId={track.id}
                cards={workerCards}
                revealCardId={revealCardId}
                onRemoveCard={(filteredIdx) => {
                  const original = workerCardSlots[filteredIdx]?.originalIndex;
                  if (original !== undefined) onRemoveCard(track.id, original);
                }}
              />
            ) : (
              <TrackGrid
                trackId={track.id}
                cards={workerCards}
                revealCardId={revealCardId}
                onRemoveCard={(filteredIdx) => {
                  const original = workerCardSlots[filteredIdx]?.originalIndex;
                  if (original !== undefined) onRemoveCard(track.id, original);
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
    </TrackContext.Provider>
  );
}
