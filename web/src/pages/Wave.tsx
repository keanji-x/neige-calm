import { useEffect, useRef, useState } from 'react';
import { Icon } from '../Icon';
import { AddPanel, type AddPanelKind } from '../shared/components/AddPanel';
import type { Cove, Route, Wave } from '../types';
import { WaveGrid } from '../WaveGrid';
import { DeleteButton } from './_shared';

// ============================================================
// WavePage — workbench: thin header + stacked cards.
// Drag is restricted to the ⠿ grip so xterm / inputs inside cards stay usable.
// ============================================================

export function WavePage({
  wave,
  cove,
  onGo,
  onAddCard,
  onRemoveCard,
  onRenameWave,
  onDeleteWave,
}: {
  wave: Wave;
  cove: Cove;
  onGo: (r: Route) => void;
  onAddCard: (waveId: string, type: AddPanelKind) => void;
  onRemoveCard: (waveId: string, idx: number) => void;
  onRenameWave?: (waveId: string, title: string) => void | Promise<void>;
  onDeleteWave?: (waveId: string) => void | Promise<void>;
}) {
  const pct = Math.round(wave.progress * 100);
  const cards = wave.cards || [];

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
        <WaveGrid
          waveId={wave.id}
          cards={cards}
          onRemoveCard={(idx) => onRemoveCard(wave.id, idx)}
        />
        <AddPanel onAdd={(type) => onAddCard(wave.id, type)} />
      </main>
    </div>
  );
}
