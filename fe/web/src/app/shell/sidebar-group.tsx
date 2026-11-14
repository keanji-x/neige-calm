import { EmptyState, type EmptyStateSchema } from '../../ui/empty-state/public.tsx';
import type { DialogOrigin } from '../../ui/dialog/public.tsx';
// Shared disclosure and Track list for every desktop sidebar group.
import { useEffect, useRef, type ReactNode } from 'react';
import { DropdownMenu, DropdownMenuItem, DropdownMenuDivider } from '@astryxdesign/core/DropdownMenu';
import type { SidebarMove } from '../../../../core/view/sidebar-layout.ts';
import { useCollapsible } from '@astryxdesign/core/Collapsible';

import { areaOf, type Area } from '../../../../core/domain/area.ts';
import { AREA_TRACK_LIMIT, limitAreaTracks, type Track } from '../../../../core/domain/track.ts';
import { TrackRow } from '../../features/track/row/public.tsx';
import { SpringRotation } from '../../ui/motion/rotation.tsx';
import { Icon, type IconName } from '../../ui/icon/public.tsx';
import { ListText } from '../../ui/list-typography/public.tsx';
import { useState } from '../../ui/state/public.ts';
import type { NavTarget } from '../router/navigation.ts';
import styles from './shell.module.css';
import { navigationRowClass as rowLayout, navigationRowPresentation, type NavigationEntrySpec } from '../../ui/navigation-row/public.ts';
import ownStyles from './area-group.module.css';

/** Reveal both shell-owned scrollports without moving an embedding host. */
export function revealSidebarRow(element: Element | null | undefined) {
  const options = { block: 'nearest', container: 'nearest' } as ScrollIntoViewOptions & { container: 'nearest' };
  element?.closest('[data-nc-sidebar-scrollport]')?.scrollIntoView?.(options);
  element?.scrollIntoView?.(options);
}

export type RowProps = Readonly<{
  isUnread: (track: Track) => boolean;
  onGo: (target: NavTarget) => void;
  nowMs?: number;
  onSetPinned: (trackId: string, pinned: boolean) => void;
  onDelete?: (trackId: string, origin: DialogOrigin) => void;
  trackActions?: (track: Track) => NonNullable<Parameters<typeof TrackRow>[0]['actions']>;
  trackRowRef?: (trackId: string, element: HTMLButtonElement | null) => void;
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
  icon: IconName;
  expanded: boolean;
  onToggle: (expanded: boolean) => void;
  level: 'section' | 'area';
  management: GroupManagement;
  extraMenuItems?: (origin: DialogOrigin) => ReactNode;
  actions?: ReactNode;
  disclosureRef?: (element: HTMLButtonElement | null) => void;
}>;

/** Group geometry, disclosure and action slots; hosts supply membership and actions. */
export function SidebarGroup({ title, label, icon, expanded, onToggle, level, management, extraMenuItems, actions, disclosureRef, children }: GroupProps & {
  children: ReactNode;
}) {
  const actionRef = useRef<HTMLSpanElement | null>(null);
  const [menuOpen, setMenuOpen] = useState(false);
  const presentation = navigationRowPresentation({ kind: 'entry', depth: 0 });
  const disclosure = useCollapsible({ isCollapsible: { isOpen: expanded, onOpenChange: onToggle } });
  const button = <button
    ref={disclosureRef}
    type="button"
    data-nc-role="row"
    className={`${styles.areaRow} ${styles.folderRow} ${rowLayout('content')} ${rowLayout('twoActions')}`}
    aria-expanded={disclosure.isOpen}
    aria-label={`${disclosure.isOpen ? 'Collapse' : 'Expand'} ${label}`}
    onClick={disclosure.toggle}
  >
    <span className={`${styles.folderIcon} ${rowLayout('leading')}`} aria-hidden="true">
      <Icon name={icon} size="md" />
    </span>
    <span className={styles.folderTitleWrap}>
      <ListText tone={presentation.tone} emphasis="medium" className={styles.areaName} title={title} fadeOverflow>{title}</ListText>
      <SpringRotation className={styles.inlineChevron} angle={disclosure.isOpen ? 90 : 0}>
        <Icon name="chevron-right" size="sm" />
      </SpringRotation>
    </span>
  </button>;
  return <div role="group" aria-label={label} className={level === 'section' ? styles.section : styles.areaGroup}>
    <div className={`${styles.areaRowWrap} ${presentation.className}`}>
      {level === 'section' ? <h2 className={styles.groupHeading}>{button}</h2> : button}
      <span ref={actionRef} className={`${styles.areaActions} ${rowLayout('inner')}`}>
        <DropdownMenu isMenuOpen={menuOpen} onOpenChange={setMenuOpen} placement="below" button={{
          label: management.menuLabel, icon: <Icon name="more" size="sm" />,
          isIconOnly: true, variant: 'ghost', size: 'sm', className: styles.areaActionsButton,
        }}>
          {menuOpen && <>
            <DropdownMenuItem label="Move up" isDisabled={!management.canMoveUp} onClick={() => management.onMove('up')} />
          <DropdownMenuItem label="Move down" isDisabled={!management.canMoveDown} onClick={() => management.onMove('down')} />
          <DropdownMenuItem label="Hide group" onClick={management.onHide} />
          {extraMenuItems !== undefined && <><DropdownMenuDivider />{extraMenuItems({ kind: 'anchor', anchor: { ref: actionRef }, unavailable: 'surface-center' })}</>}
          </>}
        </DropdownMenu>
      </span>
      {actions}
    </div>
    {disclosure.isOpen && children}
  </div>;
}

/** One list implementation: retain the active row, limit/reveal, navigate, pin and delete. */
export function SidebarTrackGroup({ emptyState, tracks, activeTrackId, areas, markCurrent = true, onGo, nowMs, onSetPinned, onDelete, isUnread, trackActions, trackRowRef, ...group }: GroupProps & RowProps & {
  emptyState: EmptyStateSchema | null;
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
  const entryPresentation: NavigationEntrySpec = { kind: 'entry', depth: 1 };
  const revealPresentation = navigationRowPresentation(entryPresentation);
  useEffect(() => {
    if (!pendingRevealRef.current) return;
    pendingRevealRef.current = false;
    revealSidebarRow(toggleRef.current);
  }, [showAll]);

  return <SidebarGroup {...group}>
    {tracks.length === 0 && emptyState !== null && <div className={styles.emptyArea}><EmptyState {...emptyState} variant="compact" /></div>}
    {tracks.length > 0 && <div className={`${styles.trackList} ${rowLayout('guide')}`}>
      {rows.map((track) => <TrackRow
        key={track.id}
        track={track}
        unread={isUnread(track)}
        areaName={areas === undefined ? undefined : areaOf(track.areaId, areas)?.name}
        variant="rail"
        presentation={entryPresentation}
        nowMs={nowMs}
        active={markCurrent && track.id === activeTrackId}
        onOpen={(trackId) => onGo({ name: 'track', trackId })}
        onSetPinned={onSetPinned}
        onDelete={onDelete}
        actions={trackActions?.(track)}
        rowRef={trackRowRef === undefined ? undefined : (element) => trackRowRef(track.id, element)}
      />)}
      {limited.hiddenCount > 0 && <button
        ref={toggleRef}
        type="button"
        data-nc-role="row"
        className={`${ownStyles.showMore} ${revealPresentation.className}`}
        aria-label={showAll ? `Show less in ${group.title}` : `Show ${limited.hiddenCount} more in ${group.title}`}
        onClick={() => { pendingRevealRef.current = showAll; setShowAll(!showAll); }}
      ><ListText tone={revealPresentation.tone} className={ownStyles.showMoreLabel}>{showAll ? 'Show less' : `Show ${limited.hiddenCount} more`}</ListText></button>}
    </div>}
  </SidebarGroup>;
}
