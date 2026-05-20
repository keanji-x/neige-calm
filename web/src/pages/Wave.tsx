import { lazy, Suspense, useEffect, useRef } from 'react';
import { useState } from '../shared/state';
import { Icon } from '../Icon';
import { AddPanel, type AddPanelKind } from '../shared/components/AddPanel';
import type { AddPanelMenuItem } from '../shared/components/AddPanel';
import type { Cove, FsmState, Route, Wave, WaveCardSlot } from '../types';
import { Modal } from '../shared/components/Modal';
import { SchemaForm } from '../shared/components/SchemaForm';
import { DirectoryBrowser } from '../shared/components/DirectoryPicker';
import { CardStatusDot } from '../shared/components/CardStatusDot';
import { DeleteButton } from './_shared';

// WaveGrid pulls in `react-grid-layout` (~50 KB minified) and is the
// heaviest single dependency on this page. Loading it lazily keeps the
// Wave route chunk small and means an empty wave (no cards yet) doesn't
// pay the grid cost at all on first paint. The flash on first navigation
// is intentional: we'd rather ship a smaller chunk than block render.
const WaveGrid = lazy(() =>
  import('../WaveGrid').then((m) => ({ default: m.WaveGrid })),
);

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
  useEffect(() => {
    if (!editingTitle) setDraftTitle(wave.title);
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
    setEditingTitle(false);
    if (!trimmed || trimmed === wave.title || !onRenameWave) return;
    await onRenameWave(wave.id, trimmed);
  };

  // Show eta pill only when there's text to show.
  const showEtaPill = !!wave.eta;
  const showPct = wave.progress > 0 && wave.progress < 1.0;

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
          <a className="wave-cove" onClick={() => onGo({ name: 'cove', coveId: cove.id })}>
            {cove.name}
          </a>
          <span className="wave-sep">·</span>
          {editingTitle ? (
            <input
              ref={titleInputRef}
              className="wave-title"
              value={draftTitle}
              onChange={(e) => setDraftTitle(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') void commitRename();
                else if (e.key === 'Escape') setEditingTitle(false);
              }}
              onBlur={() => void commitRename()}
              aria-label="Wave title"
              style={{
                background: 'transparent',
                border: 'none',
                outline: 'none',
                font: 'inherit',
                padding: 0,
                margin: 0,
                minWidth: 120,
              }}
            />
          ) : (
            <span
              className="wave-title"
              onClick={onRenameWave ? startRename : undefined}
              style={onRenameWave ? { cursor: 'text' } : undefined}
              title={onRenameWave ? 'Click to rename' : undefined}
            >
              {wave.title}
            </span>
          )}
        </span>
        <span className="wave-meta">
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
              confirmMessage={`Delete wave "${wave.title}"? Its cards (including any terminals) go too. This cannot be undone.`}
              onDelete={() => onDeleteWave(wave.id)}
            />
          )}
        </span>
      </header>

      <main className="workbench-main">
        <Suspense fallback={<div className="synth">Loading grid…</div>}>
          <WaveGrid
            waveId={wave.id}
            cards={cards}
            onRemoveCard={(idx) => onRemoveCard(wave.id, idx)}
          />
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
            <Modal
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
            </Modal>
          );
        }
        return (
          <Modal
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
          </Modal>
        );
      })()}
    </div>
  );
}
