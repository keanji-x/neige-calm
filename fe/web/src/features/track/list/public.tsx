// The track listing the area page renders.
//
// Presentational: tracks and areas arrive as props and every mutation leaves
// through a callback, because `features/**` may not reach into `app/**`.
//
// Two things are deliberately *not* re-implemented here:
//   - the ordering, which is `sortByLifecycleRank` in core (waiting → running
//     → quiet). Every surface that lists tracks has to agree on it.
//   - the row, which is `TrackRow` (INV-DUP-009).

import { areaOf, type Area } from '../../../../../core/domain/area.ts';
import { sortByLifecycleRank, visibleTracks, type Track } from '../../../../../core/domain/track.ts';
import { PanelEmpty } from '../../../ui/panel-card/public.tsx';
import { TrackRow } from '../row/public.tsx';
import styles from './list.module.css';

export type TrackListProps = Readonly<{
  tracks: readonly Track[];
  /** Used for the per-row area name/colour lookup; ignored unless `showArea`. */
  areas: readonly Area[];
  showArea?: boolean;
  activeTrackId?: string | null;
  onOpenTrack: (trackId: string) => void;
  /** Supplying this reveals each row's pin button. */
  onSetPinned?: (trackId: string, pinned: boolean) => void;
  /** Supplying this reveals each row's delete button. The caller owns the confirm. */
  onDeleteTrack?: (trackId: string) => void;
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

export function TrackList({
  tracks, areas, showArea = false, activeTrackId = null,
  onOpenTrack, onSetPinned, onDeleteTrack, nowMs, emptyMessage, variant = 'default',
}: TrackListProps) {
  const ordered = sortByLifecycleRank(visibleTracks(tracks));

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
    <ul className={styles.list} data-nc-track-list="">
      {ordered.map((track) => {
        // Unknown area ids are real: a track can outlive the area read that
        // populated `areas`. Falling back keeps the row renderable.
        const area = showArea ? areaOf(track.areaId, areas) : undefined;
        return (
          <li key={track.id} className={styles.item}>
            {/* No identity dot per row — every row in this list belongs to the
                same area, and the page header already says which (§8.2). */}
            <TrackRow
              track={track}
              variant={variant}
              areaName={showArea ? area?.name ?? 'Unknown area' : undefined}
              nowMs={nowMs}
              active={track.id === activeTrackId}
              onOpen={onOpenTrack}
              onSetPinned={onSetPinned}
              onDelete={onDeleteTrack}
            />
          </li>
        );
      })}
    </ul>
  );
}
