// Shared disclosure and Track list for every desktop sidebar group.
import { useEffect, useRef, type ReactNode } from 'react';
import { DropdownMenu, DropdownMenuItem, DropdownMenuDivider } from '@astryxdesign/core/DropdownMenu';
import type { SidebarMove } from '../../../../core/view/sidebar-layout.ts';
import { useCollapsible } from '@astryxdesign/core/Collapsible';

import { areaOf, type Area } from '../../../../core/domain/area.ts';
import { AREA_TRACK_LIMIT, limitAreaTracks, type Track } from '../../../../core/domain/track.ts';
import { TrackRow } from '../../features/track/row/public.tsx';
import { SpringRotation } from '../../ui/motion/rotation.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { ListText } from '../../ui/list-typography/public.tsx';
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
  trackActions?: (track: Track) => NonNullable<Parameters<typeof TrackRow>[0]['actions']>;
}>;

export type GroupManagement = Readonly<{
  menuLabel: string;
  canMoveUp: boolean;
  canMoveDown: boolean;
  onMove: (direction: SidebarMove) => void;
  onHide: () => void;
}>;

type GroupProps = Readonly<{
  title: string;
  /** Distinguishes an Area disclosure from a workspace-wide group. */
  label: string;
  expanded: boolean;
  onToggle: (expanded: boolean) => void;
  level: 'section' | 'area';
  management: GroupManagement;
  extraMenuItems?: ReactNode;
  actions?: ReactNode;
  disclosureRef?: (element: HTMLButtonElement | null) => void;
}>;

/** Group geometry, disclosure and action slots; hosts supply membership and actions. */
export function SidebarGroup({ title, label, expanded, onToggle, level, management, extraMenuItems, actions, disclosureRef, children }: GroupProps & {
  children: ReactNode;
}) {
  const disclosure = useCollapsible({ isCollapsible: { isOpen: expanded, onOpenChange: onToggle } });
  const button = <button
    ref={disclosureRef}
    type="button"
    data-nc-role="row"
    className={styles.areaRow}
    aria-expanded={disclosure.isOpen}
    aria-label={`${disclosure.isOpen ? 'Collapse' : 'Expand'} ${label}`}
    onClick={disclosure.toggle}
  >
    <SpringRotation className={styles.chevron} angle={disclosure.isOpen ? 90 : 0}>
      <Icon name="chevron-right" />
    </SpringRotation>
    <ListText tone={level === 'section' ? 'section' : 'group'} className={styles.areaName} title={title} fadeOverflow>{title}</ListText>
  </button>;
  return <div role="group" aria-label={label} className={level === 'section' ? styles.section : styles.areaGroup}>
    <div className={styles.areaRowWrap}>
      {level === 'section' ? <ListText as="h2" tone="section" className={styles.groupHeading}>{button}</ListText> : button}
      <span className={styles.areaActions}>
        <DropdownMenu placement="below" button={{
          label: management.menuLabel, icon: <Icon name="more" size="sm" />,
          isIconOnly: true, variant: 'ghost', size: 'sm', className: styles.areaActionsButton,
        }}>
          <DropdownMenuItem label="Move up" isDisabled={!management.canMoveUp} onClick={() => management.onMove('up')} />
          <DropdownMenuItem label="Move down" isDisabled={!management.canMoveDown} onClick={() => management.onMove('down')} />
          <DropdownMenuItem label="Hide group" onClick={management.onHide} />
          {extraMenuItems !== undefined && <><DropdownMenuDivider />{extraMenuItems}</>}
        </DropdownMenu>
      </span>
      {actions}
    </div>
    {disclosure.isOpen && children}
  </div>;
}

/** One list implementation: retain the active row, limit/reveal, navigate, pin and delete. */
export function SidebarTrackGroup({ tracks, activeTrackId, areas, markCurrent = true, onGo, nowMs, onSetPinned, onDelete, isUnread, trackActions, ...group }: GroupProps & RowProps & {
  tracks: readonly Track[];
  activeTrackId: string | null;
  /** Cross-Area groups identify the Area on each row; Area groups omit this. */
  areas?: readonly Area[];
  /** Only the canonical Area row owns aria-current; shortcuts still retain the active Track past the limit. */
  markCurrent?: boolean;
}) {
  const [showAll, setShowAll] = useState(false);
  const toggleRef = useRef<HTMLButtonElement | null>(null);
  const pendingRevealRef = useRef(false);
  const limited = limitAreaTracks(tracks, AREA_TRACK_LIMIT, activeTrackId);
  const rows = showAll ? tracks : limited.rows;
  useEffect(() => {
    if (!pendingRevealRef.current) return;
    pendingRevealRef.current = false;
    toggleRef.current?.scrollIntoView?.({ block: 'nearest' });
  }, [showAll]);

  return <SidebarGroup {...group}>
    {tracks.length > 0 && <div className={styles.trackList}>
      {rows.map((track) => <TrackRow
        key={track.id}
        track={track}
        unread={isUnread(track)}
        areaName={areas === undefined ? undefined : areaOf(track.areaId, areas)?.name}
        variant="rail"
        nowMs={nowMs}
        active={markCurrent && track.id === activeTrackId}
        onOpen={(trackId) => onGo({ name: 'track', trackId })}
        onSetPinned={onSetPinned}
        onDelete={onDelete}
        actions={trackActions?.(track)}
      />)}
      {limited.hiddenCount > 0 && <button
        ref={toggleRef}
        type="button"
        data-nc-role="row"
        className={ownStyles.showMore}
        aria-label={showAll ? `Show less in ${group.title}` : `Show ${limited.hiddenCount} more in ${group.title}`}
        onClick={() => { pendingRevealRef.current = showAll; setShowAll(!showAll); }}
      >{showAll ? 'Show less' : `Show ${limited.hiddenCount} more`}</button>}
    </div>}
  </SidebarGroup>;
}
