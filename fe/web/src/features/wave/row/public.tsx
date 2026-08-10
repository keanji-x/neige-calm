// INV-DUP-009 — the one wave row.
//
// Three surfaces render it: the sidebar rail, the cove page list, and any
// future wave picker. They must agree on what a wave *looks* like, so the row
// is declared once here and consumed through this entry. `app/**` may import
// it; sibling feature domains may not (cross-domain imports are forbidden) —
// the cove page gets its list composed by `app/router`, not by importing this
// directly.

import {
  isRunning, lifecycleLabel, needsUserAttention, waveDisplayTitle, type Wave,
} from '../../../../../core/domain/wave.ts';
import styles from './row.module.css';

export type WaveRowProps = Readonly<{
  wave: Wave;
  /** Cove name, when the surface does not already group by cove. */
  coveName?: string;
  coveColor?: string;
  active?: boolean;
  compact?: boolean;
  onOpen: (waveId: string) => void;
  /** Supplying this reveals a pin button. See INV-SIDEBAR-012 below. */
  onSetPinned?: (waveId: string, pinned: boolean) => void;
  /** Supplying this reveals a delete button. The caller owns the confirm. */
  onDelete?: (waveId: string) => void;
}>;

/**
 * The row is a `<button>` and the pin/delete affordances are **siblings**, not
 * children: nesting interactive elements is invalid HTML and trips axe's
 * `nested-interactive`. A positioning wrapper owns the hover reveal, and the
 * row reserves a right gutter so the buttons never overlap the title.
 *
 * INV-SIDEBAR-012 — the pin button is hover-revealed **except when the wave is
 * already pinned**, where it stays permanently visible. Touch has no hover, so
 * a hover-only unpin would be unreachable on a tablet: once pinned, the only
 * way back out has to be visible.
 *
 * INV-A11Y-061 — navigation is this button plus `onOpen`, never an `<a href>`.
 */
export function WaveRow({
  wave, coveName, coveColor, active = false, compact = false, onOpen, onSetPinned, onDelete,
}: WaveRowProps) {
  const attention = needsUserAttention(wave);
  const running = isRunning(wave.lifecycle);
  const pinned = wave.pinnedAt !== null;
  const title = waveDisplayTitle(wave.title);
  const lifecycle = lifecycleLabel(wave.lifecycle);
  const showProgress = running && wave.progress > 0;

  const bits = [attention ? 'waiting on you' : '', running ? 'running' : ''].filter(Boolean);
  const label = `Wave ${title}${bits.length > 0 ? `, ${bits.join(', ')}` : ''}, ${lifecycle}`
    + (coveName === undefined ? '' : `, in cove ${coveName}`);

  return (
    <div className={`${styles.wrapper} ${compact ? styles.wrapperCompact : ''}`}>
      <button
        type="button"
        className={`${styles.row} ${active ? styles.rowActive : ''} ${attention ? styles.rowAttention : ''}`}
        aria-current={active ? 'page' : undefined}
        aria-label={label}
        onClick={() => onOpen(wave.id)}
      >
        <span
          className={`${styles.glyph} ${attention ? styles.glyphAttention : running ? styles.glyphRunning : ''}`}
          aria-hidden="true"
        />
        <span className={styles.body}>
          <span className={styles.titleRow}>
            <span className={styles.title}>{title}</span>
            {!compact && <span className={styles.lifecycle}>{lifecycle}</span>}
          </span>
          {!compact && (coveName !== undefined || wave.now !== '') && (
            <span className={styles.meta}>
              {coveName !== undefined && (
                <span className={styles.coveTag}>
                  <span className={styles.coveDot} style={{ background: coveColor }} aria-hidden="true" />
                  {coveName}
                </span>
              )}
              {/* Only emit the separator when both halves render, or a wave
                  with no plugin activity shows a stranded middot. */}
              {coveName !== undefined && wave.now !== '' && <span aria-hidden="true">·</span>}
              {wave.now !== '' && <span className={styles.now}>{wave.now}</span>}
            </span>
          )}
          {!compact && showProgress && (
            <span className={styles.progressTrack} aria-hidden="true">
              <span
                className={styles.progressFill}
                style={{ inlineSize: `${Math.round(Math.min(1, Math.max(0, wave.progress)) * 100)}%` }}
              />
            </span>
          )}
        </span>
      </button>

      {onSetPinned !== undefined && (
        <button
          type="button"
          className={`${styles.action} ${styles.pin} ${pinned ? styles.pinAlways : ''}`}
          aria-label={pinned ? `Unpin ${title}` : `Pin ${title}`}
          aria-pressed={pinned}
          onClick={() => onSetPinned(wave.id, !pinned)}
        >
          {pinned ? '★' : '☆'}
        </button>
      )}
      {onDelete !== undefined && (
        <button
          type="button"
          className={`${styles.action} ${styles.remove}`}
          aria-label={`Delete ${title}`}
          onClick={() => onDelete(wave.id)}
        >
          ×
        </button>
      )}
    </div>
  );
}
