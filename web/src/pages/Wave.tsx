import { lazy, Suspense, useEffect, useRef, useState } from 'react';
import { Icon } from '../Icon';
import { AddPanel, type AddPanelKind } from '../shared/components/AddPanel';
import type { AddPanelMenuItem } from '../shared/components/AddPanel';
import type { Cove, Route, Wave, WaveCardSlot } from '../types';
import { registerConfigCardHandlers } from '../cards/builtins/config';
import { DeleteButton } from './_shared';

// WaveGrid pulls in `react-grid-layout` (~50 KB minified) and is the
// heaviest single dependency on this page. Loading it lazily keeps the
// Wave route chunk small and means an empty wave (no cards yet) doesn't
// pay the grid cost at all on first paint. The flash on first navigation
// is intentional: we'd rather ship a smaller chunk than block render.
const WaveGrid = lazy(() =>
  import('../WaveGrid').then((m) => ({ default: m.WaveGrid })),
);

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
  // Local-only transient slots (today: a "config card" rendered after the
  // user picks a kind that declares a `createSchema`). These never reach
  // the kernel; submitting / cancelling clears the slot. Tracked outside
  // the kernel-derived `wave.cards` so they don't fight WS invalidations.
  const [transientSlot, setTransientSlot] = useState<{
    slot: WaveCardSlot;
    targetKind: string;
  } | null>(null);
  const cards: WaveCardSlot[] = transientSlot
    ? [...(wave.cards || []), transientSlot.slot]
    : wave.cards || [];

  const beginAdd = (item: AddPanelMenuItem) => {
    if (!item.createSchema) {
      // Legacy "create immediately" path — current terminal flow.
      onAddCard(wave.id, item.type);
      return;
    }
    // Schema-driven: drop in a transient config card and register
    // submit/cancel handlers keyed by its id.
    const id = `__config-${item.type}-${Date.now().toString(36)}`;
    const slot: WaveCardSlot = {
      kind: 'card',
      card: { type: 'config', id, targetKind: item.type },
    };
    const unregister = registerConfigCardHandlers(id, {
      onCancel: () => {
        unregister();
        setTransientSlot(null);
      },
      onSubmit: async (values) => {
        try {
          await onCreateCardWithBody?.(wave.id, item.type, values);
        } finally {
          unregister();
          setTransientSlot(null);
        }
      },
    });
    setTransientSlot({ slot, targetKind: item.type });
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
            onRemoveCard={(idx) => {
              // The transient config card sits at the tail of the slot
              // list (above) — close it locally without going through
              // the kernel-card delete path.
              const transientIdx = transientSlot ? cards.length - 1 : -1;
              if (idx === transientIdx) {
                setTransientSlot(null);
                return;
              }
              onRemoveCard(wave.id, idx);
            }}
          />
        </Suspense>
        <AddPanel onSelect={beginAdd} />
      </main>
    </div>
  );
}
