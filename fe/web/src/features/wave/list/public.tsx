// The wave listing the cove page renders.
//
// Presentational: waves and coves arrive as props and every mutation leaves
// through a callback, because `features/**` may not reach into `app/**`.
//
// Two things are deliberately *not* re-implemented here:
//   - the ordering, which is `sortByLifecycleRank` in core (waiting → running
//     → quiet). Every surface that lists waves has to agree on it.
//   - the row, which is `WaveRow` (INV-DUP-009).

import { coveOf, type Cove } from '../../../../../core/domain/cove.ts';
import { sortByLifecycleRank, visibleWaves, type Wave } from '../../../../../core/domain/wave.ts';
import { PanelEmpty } from '../../../ui/panel-card/public.tsx';
import { WaveRow } from '../row/public.tsx';
import styles from './list.module.css';

export type WaveListProps = Readonly<{
  waves: readonly Wave[];
  /** Used for the per-row cove name/colour lookup; ignored unless `showCove`. */
  coves: readonly Cove[];
  showCove?: boolean;
  activeWaveId?: string | null;
  onOpenWave: (waveId: string) => void;
  /** Supplying this reveals each row's pin button. */
  onSetPinned?: (waveId: string, pinned: boolean) => void;
  /** Supplying this reveals each row's delete button. The caller owns the confirm. */
  onDeleteWave?: (waveId: string) => void;
  nowMs?: number;
  emptyMessage: string;
  /**
   * §6.3's variant. `panel` inside a 308px panel module, `default` when the
   * list owns a main column. The two-line variant in a panel is 48px of row for
   * a lifecycle phrase the panel has no width to set — and its hover actions
   * sit at the row's own edge, which in a panel is the card's edge.
   */
  variant?: 'default' | 'compact' | 'panel';
}>;

export function WaveList({
  waves, coves, showCove = false, activeWaveId = null,
  onOpenWave, onSetPinned, onDeleteWave, nowMs, emptyMessage, variant = 'default',
}: WaveListProps) {
  const ordered = sortByLifecycleRank(visibleWaves(waves));

  if (ordered.length === 0) {
    // The same empty state the conversation module renders, because the two sit
    // one above the other in the same card. This used to be a dashed box at
    // `--radius-md` — the card's own radius — so an object was drawing a corner
    // equal to the corner of the thing containing it, and the inner one read as
    // tighter. It also paid `--space-4` of padding inside a body that already
    // pays `--space-4`. One empty-state vocabulary, no box.
    return <PanelEmpty>{emptyMessage}</PanelEmpty>;
  }

  return (
    <ul className={styles.list} data-nc-wave-list="">
      {ordered.map((wave) => {
        // Unknown cove ids are real: a wave can outlive the cove read that
        // populated `coves`. Falling back keeps the row renderable.
        const cove = showCove ? coveOf(wave.coveId, coves) : undefined;
        return (
          <li key={wave.id} className={styles.item}>
            {/* No identity dot per row — every row in this list belongs to the
                same cove, and the page header already says which (§8.2). */}
            <WaveRow
              wave={wave}
              variant={variant}
              coveName={showCove ? cove?.name ?? 'Unknown cove' : undefined}
              nowMs={nowMs}
              active={wave.id === activeWaveId}
              onOpen={onOpenWave}
              onSetPinned={onSetPinned}
              onDelete={onDeleteWave}
            />
          </li>
        );
      })}
    </ul>
  );
}
