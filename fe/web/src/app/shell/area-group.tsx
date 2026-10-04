// Area-owned membership and actions, composed into the shared sidebar group.
import { DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';

import type { Area } from '../../../../core/domain/area.ts';
import { railAreaTracks, type Track } from '../../../../core/domain/track.ts';
import { Icon } from '../../ui/icon/public.tsx';
import { SidebarTrackGroup, type RowProps, type GroupManagement } from './sidebar-group.tsx';
import styles from './shell.module.css';

export function AreaGroup({
  area, areaTracks, activeTrackId, expanded, onToggle, showClosed, onSetShowClosed, disclosureRef, onEdit,
  onRequestDelete, onNewTrack, management, ...rowProps
}: RowProps & {
  management: GroupManagement;
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
  return <SidebarTrackGroup
    title={area.name}
    label={`area ${area.name}`}
    level="area"
    tracks={railAreaTracks(areaTracks, activeTrackId, rowProps.isUnread, showClosed)}
    activeTrackId={activeTrackId}
    expanded={expanded}
    onToggle={onToggle}
    disclosureRef={disclosureRef}
    management={management}
    extraMenuItems={<>
      <DropdownMenuItem label="Edit area" onClick={onEdit} />
      <DropdownMenuItem label={showClosed ? 'Hide closed' : 'Show closed'} onClick={() => onSetShowClosed(!showClosed)} />
      <DropdownMenuItem label="Delete area" onClick={() => onRequestDelete(area.id)} />
    </>}
    actions={
      <button type="button" data-nc-role="icon" className={styles.areaNew}
        aria-label={`New track in ${area.name}`} title="New track" onClick={() => onNewTrack(area.id)}>
        <Icon name="plus" size="sm" />
      </button>
    }
    {...rowProps}
  />;
}
