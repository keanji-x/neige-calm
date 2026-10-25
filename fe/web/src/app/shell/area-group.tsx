import type { DialogOrigin } from '../../ui/dialog/public.tsx';
import { GuideAnchor } from '../../ui/floating-control/public.ts';
// Area-owned membership and actions, composed into the shared sidebar group.
import { DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';

import type { Area } from '../../../../core/domain/area.ts';
import { areaPinnedTracks, railAreaTracks, type Track } from '../../../../core/domain/track.ts';
import { Icon } from '../../ui/icon/public.tsx';
import { SidebarTrackGroup, type RowProps, type GroupManagement } from './sidebar-group.tsx';
import styles from './shell.module.css';
import { navigationRowClass as rowLayout } from '../../ui/navigation-row/public.ts';
import { useUiPreferences } from '../providers/ui-preferences.tsx';

export function AreaGroup({
  readReady, area, areaTracks, activeTrackId, expanded, onToggle, showClosed, onSetShowClosed, disclosureRef, onEdit,
  onRequestDelete, onNewTrack, management, ...rowProps
}: RowProps & {
  readReady: boolean;
  management: GroupManagement;
  area: Area;
  areaTracks: readonly Track[];
  activeTrackId: string | null;
  expanded: boolean;
  onToggle: (expanded: boolean) => void;
  showClosed: boolean;
  onSetShowClosed: (showClosed: boolean) => void;
  disclosureRef: (element: HTMLButtonElement | null) => void;
  onEdit: (origin: DialogOrigin) => void;
  onRequestDelete: (areaId: string, origin: DialogOrigin) => void;
  onNewTrack: (areaId: string) => void;
}) {
  const preferences = useUiPreferences();
  const ordered = areaPinnedTracks(areaTracks, (track) => preferences.areaTrackPinned(area.id, track.id));
  return <SidebarTrackGroup
    emptyState={readReady ? { icon: <Icon name="file" />, title: '没有可显示的 Track', description: '此 Area 中符合当前筛选的 Track 会显示在这里。' } : null}
    title={area.name}
    label={`area ${area.name}`}
    level="area"
    tracks={railAreaTracks(ordered, activeTrackId, rowProps.isUnread, showClosed)}
    activeTrackId={activeTrackId}
    expanded={expanded}
    onToggle={onToggle}
    disclosureRef={disclosureRef}
    management={management}
    extraMenuItems={(origin) => <>
      <DropdownMenuItem label="Edit area" onClick={() => onEdit(origin)} />
      <DropdownMenuItem label={showClosed ? 'Hide closed' : 'Show closed'} onClick={() => onSetShowClosed(!showClosed)} />
      <DropdownMenuItem label="Delete area" onClick={() => onRequestDelete(area.id, origin)} />
    </>}
    actions={
      <GuideAnchor id="workspace-new-track">{(anchor, describedBy) => <button ref={anchor} aria-describedby={describedBy} type="button" data-nc-role="icon" className={`${styles.areaNew} ${rowLayout('outer')}`}
        aria-label={`New track in ${area.name}`} title="New track" onClick={() => onNewTrack(area.id)}>
        <Icon name="plus" size="sm" />
      </button>}</GuideAnchor>
    }
    {...rowProps}
  />;
}
