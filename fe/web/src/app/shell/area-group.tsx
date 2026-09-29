// One Area in the desktop rail: its disclosure row and its most recent Tracks.

import { ListText } from '../../ui/list-typography/public.tsx';
import { useEffect, useRef } from 'react';
import { useCollapsible } from '@astryxdesign/core/Collapsible';
import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';

import type { Area } from '../../../../core/domain/area.ts';
import {
  AREA_TRACK_LIMIT, limitAreaTracks, railAreaTracks, type Track,
} from '../../../../core/domain/track.ts';
import { TrackRow } from '../../features/track/row/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { useState } from '../../ui/state/public.ts';
import type { NavTarget } from '../router/navigation.ts';
import styles from './shell.module.css';
import ownStyles from './area-group.module.css';

export type RowProps = Readonly<{
  isUnread: (track: Track) => boolean;
  onGo: (target: NavTarget) => void;
  nowMs?: number;
  onSetPinned: (trackId: string, pinned: boolean) => void;
  onDelete: (trackId: string) => void;
}>;

/**
 * Navigation is `<button>` + `onGo`, never `<a href>`, the `+` included: this rail
 * does not mix the two activation models. The Area row is a disclosure, not
 * navigation. `+` and the actions menu are both permanently visible and never
 * share a slot.
 *
 * The list shows the Area's most recent Tracks plus the open one; `Show N more`
 * reveals the rest. The choice lives in this component's memory, so it survives
 * collapsing the Area (which keeps the group mounted) and resets on reload.
 * Closed Tracks are left out first, unless unread or open, or the Area's
 * persisted `Show closed` is on.
 */
export function AreaGroup({
  area, areaTracks, activeTrackId, expanded, onToggle, showClosed, onSetShowClosed, disclosureRef, onEdit,
  onRequestDelete, onNewTrack, onGo, nowMs, onSetPinned, onDelete, isUnread,
}: RowProps & {
  area: Area;
  areaTracks: readonly Track[];
  activeTrackId: string | null;
  expanded: boolean;
  onToggle: (expanded: boolean) => void;
  showClosed: boolean;
  onSetShowClosed: (showClosed: boolean) => void;
  disclosureRef: (element: HTMLButtonElement | null) => void;
  onEdit: () => void;
  onRequestDelete: (areaId: string) => void;
  onNewTrack: (areaId: string) => void;
}) {
  const disclosure = useCollapsible({
    isCollapsible: { isOpen: expanded, onOpenChange: onToggle },
  });
  const [showAll, setShowAll] = useState(false);
  const toggleRef = useRef<HTMLButtonElement | null>(null);
  const pendingRevealRef = useRef(false);
  const railTracks = railAreaTracks(areaTracks, activeTrackId, isUnread, showClosed);
  const limited = limitAreaTracks(railTracks, AREA_TRACK_LIMIT, activeTrackId);
  const rows = showAll ? railTracks : limited.rows;

  /* `Show less` removes the rows above the focused toggle, which can leave it
       outside the rail's scrollport. Only that click asks for the reveal. */
  useEffect(() => {
    if (!pendingRevealRef.current) return;
    pendingRevealRef.current = false;
    toggleRef.current?.scrollIntoView?.({ block: 'nearest' });
  }, [showAll]);

  return (
    <div className={styles.areaGroup}>
      <div className={styles.areaRowWrap}>
        <button
          ref={disclosureRef}
          type="button"
          data-nc-role="row"
          className={styles.areaRow}
          aria-expanded={disclosure.isOpen}
          aria-label={`${disclosure.isOpen ? 'Collapse' : 'Expand'} area ${area.name}`}
          onClick={disclosure.toggle}
        >
          <span className={`${styles.chevron} ${disclosure.isOpen ? styles.chevronOpen : ''}`} aria-hidden="true">
            <Icon name="chevron-right" />
          </span>
          <ListText tone="group" className={styles.areaName} title={area.name}>{area.name}</ListText>
        </button>
        <span className={styles.areaActions}>
          <DropdownMenu
            placement="below"
            button={{
              label: `Area actions for ${area.name}`,
              icon: <Icon name="more" size="sm" />,
              isIconOnly: true,
              variant: 'ghost',
              size: 'sm',
              className: styles.areaActionsButton,
            }}
          >
            <DropdownMenuItem label="Edit area" onClick={onEdit} />
            <DropdownMenuItem
              label={showClosed ? 'Hide closed' : 'Show closed'}
              onClick={() => onSetShowClosed(!showClosed)}
            />
            <DropdownMenuItem label="Delete area" onClick={() => onRequestDelete(area.id)} />
          </DropdownMenu>
        </span>
        {/* The accessible name names the area: N controls all called "New track" is a
                    list a screen-reader user cannot choose from. `title` is the sighted hover label. */}
        <button
          type="button"
          data-nc-role="icon"
          className={styles.areaNew}
          aria-label={`New track in ${area.name}`}
          title="New track"
          onClick={() => onNewTrack(area.id)}
        >
          <Icon name="plus" size="sm" />
        </button>
      </div>
      {disclosure.isOpen && railTracks.length > 0 && (
        <div className={styles.trackList}>
          {rows.map((track) => (
            <TrackRow
              key={track.id}
              track={track}
              unread={isUnread(track)}
              variant="rail"
              nowMs={nowMs}
              active={track.id === activeTrackId}
              onOpen={(trackId) => onGo({ name: 'track', trackId })}
              onSetPinned={onSetPinned}
              onDelete={onDelete}
            />
          ))}
          {/* One element in a fixed slot whose text flips, so focus stays on it. The
                        name starts with the visible text and names the area, like `+`;
                        no `aria-expanded`, since the flipped text already says it. */}
          {limited.hiddenCount > 0 && (
            <button
              ref={toggleRef}
              type="button"
              data-nc-role="row"
              className={ownStyles.showMore}
              aria-label={showAll ? `Show less in ${area.name}` : `Show ${limited.hiddenCount} more in ${area.name}`}
              onClick={() => {
                pendingRevealRef.current = showAll;
                setShowAll(!showAll);
              }}
            >
              {showAll ? 'Show less' : `Show ${limited.hiddenCount} more`}
            </button>
          )}
        </div>
      )}
    </div>
  );
}
