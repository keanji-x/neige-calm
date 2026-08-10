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
import { sortByLifecycleRank, type Wave } from '../../../../../core/domain/wave.ts';
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
  emptyMessage: string;
}>;

export function WaveList({
  waves, coves, showCove = false, activeWaveId = null,
  onOpenWave, onSetPinned, onDeleteWave, emptyMessage,
}: WaveListProps) {
  const ordered = sortByLifecycleRank(waves);

  if (ordered.length === 0) {
    return <p className={styles.empty} data-nc-wave-list-empty="">{emptyMessage}</p>;
  }

  return (
    <ul className={styles.list} data-nc-wave-list="">
      {ordered.map((wave) => {
        // Unknown cove ids are real: a wave can outlive the cove read that
        // populated `coves`. Falling back keeps the row renderable.
        const cove = showCove ? coveOf(wave.coveId, coves) : undefined;
        return (
          <li key={wave.id} className={styles.item}>
            <WaveRow
              wave={wave}
              coveName={showCove ? cove?.name ?? 'Unknown cove' : undefined}
              coveColor={cove?.color}
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
