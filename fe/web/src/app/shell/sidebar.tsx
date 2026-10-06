// The workspace rail: shared groups with owner-defined membership and actions.

import { useEffect, useRef } from 'react';
import { DropdownMenu, DropdownMenuItem, DropdownMenuSubMenu } from '@astryxdesign/core/DropdownMenu';

import { visibleAreas, type Area } from '../../../../core/domain/area.ts';
import { DELETE_FAILURES, DELETE_TEXT, writeFailureText } from '../../../../core/domain/failure-class.ts';
import {
  TRACK_PATCH_FAILURES, TRACK_PATCH_TEXT, hasFailed, isWorking, needsUserAttention, sortAreaTracksByRecent, userVisibleTracks, type Track,
} from '../../../../core/domain/track.ts';
import { deleteAreaCopy, DELETE_TRACK_COPY } from '../../ui/confirm-dialog/copy.ts';
import { ConfirmDialog } from '../../ui/dialog/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { Menu } from '../../ui/menu/public.tsx';
import { ConnectionIndicator } from './connection-indicator.tsx';
import {
  OperationFeedback, useDeleteConfirm, useOperationFeedback,
} from '../../ui/operation-feedback/public.tsx';
import { useUiPreferences } from '../providers/ui-preferences.tsx';
import { TypedDeleteBody, useTypedConfirm } from '../../ui/typed-confirm/public.tsx';
import type { NavTarget } from '../router/navigation.ts';
import { routeParamFromPath } from '../router/navigation.ts';
import { SIDEBAR_SECTION_IDS, moveSidebarGroup, type SidebarGroupId, type SidebarMove, type SidebarSectionId } from '../../../../core/view/sidebar-layout.ts';
import { AreaGroup } from './area-group.tsx';
import { SidebarGroup, SidebarTrackGroup, type GroupManagement } from './sidebar-group.tsx';
import styles from './shell.module.css';

export type SidebarProps = Readonly<{
  areas: readonly Area[];
  tracksByArea: ReadonlyMap<string, readonly Track[]>;
  tracks: readonly Track[];
  currentPath: string;
  onGo: (target: NavTarget) => void;
  onRequestCreateArea: () => void;
  onRequestEditArea: (area: Area) => void;
  onDeleteArea: (areaId: string, signal: AbortSignal) => void | Promise<void>;
  /** Goes to the new-track page for this area, through the shell. */
  onNewTrack: (areaId: string) => void;
  onSetPinned: (trackId: string, pinned: boolean) => void | Promise<void>;
  onDeleteTrack: (trackId: string, signal: AbortSignal) => void | Promise<void>;
  onOpenSettings: () => void;
  /** Settings › Plugins, reachable without walking through Settings first. */
  onOpenPlugins: () => void;
  /** The shell never signs out itself; the owner of the session does. */
  onSignOut: () => void;
  /** Owned by the shell: collapsing changes the shell grid, not just the rail. */
  collapsed: boolean;
  onToggleCollapsed: () => void;
  userLabel?: string;
  nowMs?: number;
  readError?: string | null;
  activityError?: string | null;
  readLoading?: boolean;
  onRetryRead?: () => void;
}>;

/** Two initials at most; the avatar is decoration on top of a labelled button. */
export function initialsOf(label: string): string {
  const parts = label.split(/\s+/).filter((part) => part.length > 0);
  return parts.slice(0, 2).map((part) => part[0]?.toUpperCase() ?? '').join('') || '?';
}

/**
 * Pinning is not relocation: a pinned track appears in Pinned and in its area's
 * list, and in "Waiting on you" too if it needs attention. There is intentionally
 * no skip-to-main link. `userVisibleTracks` filters the kernel system area here
 * as well as in the query layer.
 */
export function Sidebar({
  areas, tracksByArea, tracks, currentPath, onGo,
  onRequestCreateArea, onRequestEditArea, onDeleteArea, onNewTrack, onSetPinned, onDeleteTrack,
  onOpenSettings, onOpenPlugins, onSignOut, collapsed, onToggleCollapsed,
  userLabel = 'You', nowMs, readError = null, activityError = null,
  readLoading = false, onRetryRead = () => undefined,
}: SidebarProps) {
  const preferences = useUiPreferences();
  const railRef = useRef<HTMLElement | null>(null);
  const areaDisclosureRefs = useRef(new Map<string, HTMLButtonElement>());
  const pendingAreaFocusRef = useRef<string | null>(null);
  const pendingOptionsFocusRef = useRef(false);
  const trackConfirm = useDeleteConfirm(onDeleteTrack, writeFailureText(DELETE_FAILURES, DELETE_TEXT));
  const areaConfirm = useDeleteConfirm(onDeleteArea, writeFailureText(DELETE_FAILURES, DELETE_TEXT), () => onGo({ name: 'today' }));
  const writeFeedback = useOperationFeedback();

  const userAreas = visibleAreas(areas);
  const userTracks = userVisibleTracks(tracks, areas);
  // "Waiting on you" is what the kernel says needs a person: input or repair.
  const waiting = userTracks.filter((track) => needsUserAttention(track) || hasFailed(track));
  const pinned = userTracks.filter((track) => track.pinnedAt !== null)
    .toSorted((left, right) => (right.pinnedAt ?? 0) - (left.pinnedAt ?? 0));

  const isUnread = (track: Track) => preferences.isUnread('track', track.id, track.activityAt ?? 0);
  const groups: ReadonlyArray<{ id: SidebarSectionId; title: string; tracks: readonly Track[] }> = [
    { id: 'waiting', title: 'Waiting on you', tracks: waiting },
    { id: 'pinned', title: 'Pinned', tracks: pinned },
    { id: 'unread', title: 'Unread', tracks: sortAreaTracksByRecent(userTracks.filter(isUnread)) },
    { id: 'running', title: 'Running', tracks: sortAreaTracksByRecent(userTracks.filter(isWorking)) },
  ];

  const sectionOrder = preferences.sidebarOrder('sections', SIDEBAR_SECTION_IDS);
  const sectionVisible = (id: SidebarSectionId) => preferences.sidebarGroupVisible(id)
    && (id === 'areas' || id === 'unread' || id === 'running' || groups.find((group) => group.id === id)!.tracks.length > 0);
  const shownSections = sectionOrder.filter(sectionVisible);
  const areaOrder = preferences.sidebarOrder('areas', userAreas.map((area) => area.id));
  const orderedAreas = areaOrder.map((id) => userAreas.find((area) => area.id === id)!);
  const shownAreas = orderedAreas.filter((area) => preferences.sidebarGroupVisible(`area:${area.id}`));
  const hiddenGroups = [
    ...sectionOrder.filter((id) => !preferences.sidebarGroupVisible(id)).map((id) => ({
      id, title: id === 'areas' ? 'Areas' : groups.find((group) => group.id === id)!.title,
      restore: () => preferences.setSidebarGroupVisible(id, true),
    })),
    ...orderedAreas.filter((area) => !preferences.sidebarGroupVisible(`area:${area.id}`)).map((area) => ({
      id: `area:${area.id}` as SidebarGroupId, title: `area ${area.name}`,
      restore: () => {
        preferences.setSidebarGroupVisible('areas', true);
        preferences.setSidebarGroupExpanded('areas', true);
        preferences.setSidebarGroupVisible(`area:${area.id}`, true);
      },
    })),
  ];
  const management = (id: SidebarGroupId, menuLabel: string): GroupManagement => {
    const isArea = id.startsWith('area:');
    const key = isArea ? id.slice('area:'.length) : id;
    const shown: readonly string[] = isArea ? shownAreas.map((area) => area.id) : shownSections;
    const order = isArea ? areaOrder : sectionOrder;
    const index = shown.indexOf(key);
    return {
      menuLabel, canMoveUp: index > 0, canMoveDown: index >= 0 && index < shown.length - 1,
      onMove: (direction: SidebarMove) => preferences.setSidebarOrder(isArea ? 'areas' : 'sections', moveSidebarGroup<string>(order, shown, key, direction)),
      onHide: () => { pendingOptionsFocusRef.current = true; preferences.setSidebarGroupVisible(id, false); },
    };
  };
  useEffect(() => {
    if (!pendingOptionsFocusRef.current) return;
    pendingOptionsFocusRef.current = false;
    railRef.current?.querySelector<HTMLButtonElement>('[aria-label="Sidebar view options"]')?.focus();
  });

  const activeTrackId = routeParamFromPath(currentPath, '/track/') ?? null;

  const deletingArea = userAreas.find((area) => area.id === areaConfirm.target);
  const typed = useTypedConfirm(deletingArea?.name ?? '');
  const areaCopy = deleteAreaCopy(
    deletingArea?.name ?? '',
    tracksByArea.get(areaConfirm.target ?? '')?.length,
  );

  // Reveal the current row when visible without undoing manual Area collapse.
  const activeAreaId = userTracks.find((track) => track.id === activeTrackId)?.areaId;
  const activeAreaExpanded = activeAreaId === undefined ? true : preferences.areaExpanded(activeAreaId);
  const areasExpanded = preferences.sidebarGroupExpanded('areas');
  useEffect(() => {
    if (collapsed || activeTrackId === null) return;
    railRef.current?.querySelector('[aria-current="page"]')
      ?.scrollIntoView?.({ block: 'nearest' });
  }, [activeTrackId, collapsed, activeAreaExpanded, areasExpanded]);

  /* A collapsed Area initial is an entrance into the expanded tree: restore focus
       to the disclosure it reveals, or activating the unmounted initial drops focus
       onto <body>. Runs after the current-Track scroll above on purpose. */
  useEffect(() => {
    if (collapsed) return;
    const areaId = pendingAreaFocusRef.current;
    if (areaId === null) return;
    pendingAreaFocusRef.current = null;
    const disclosure = areaDisclosureRefs.current.get(areaId);
    disclosure?.focus({ preventScroll: true });
    disclosure?.scrollIntoView?.({ block: 'nearest' });
  }, [collapsed]);

  const setPinned = (trackId: string, next: boolean) => {
    void writeFeedback.run(Promise.resolve(onSetPinned(trackId, next)), writeFailureText(TRACK_PATCH_FAILURES, TRACK_PATCH_TEXT.pin));
  };
  const rowProps = {
    // The receipt compares the overlay's completion high-water mark, not `updatedAt`
    // (which moves on every rename and pin); `null` is never unread.
    isUnread,
    onGo: (target: NavTarget) => {
      if (target.name === 'track') {
        const track = userTracks.find((candidate) => candidate.id === target.trackId);
        if (track !== undefined) preferences.markRead('track', track.id, track.activityAt ?? 0);
      }
      onGo(target);
    },
    nowMs,
    onSetPinned: setPinned,
    onDelete: trackConfirm.request,
    trackActions: (track: Track) => ({
      areaPinned: preferences.areaTrackPinned(track.areaId, track.id),
      onSetPinned: setPinned,
      onSetAreaPinned: (id: string, next: boolean) => preferences.setAreaTrackPinned(track.areaId, id, next),
      onMarkUnread: (id: string) => preferences.markUnread('track', id),
    }),
  };

  return (
    <nav ref={railRef} className={`${styles.rail} ${collapsed ? styles.railCollapsed : ''}`} aria-label="Workspace">
      <div className={styles.brandRow}>
        {!collapsed && (
          <button
            type="button"
            data-nc-role="row"
            className={styles.brand}
            aria-label="Go to Today"
            onClick={() => onGo({ name: 'today' })}
          >
            <span className={styles.brandMark} aria-hidden="true" />
            <span className={styles.brandText}>Today</span>
          </button>
        )}
        {collapsed && (
          <button
            type="button"
            className={styles.iconButton}
            aria-label="Expand sidebar"
            aria-expanded="false"
            onClick={onToggleCollapsed}
          >
            <span className={styles.brandMark} aria-hidden="true" />
          </button>
        )}
        <ConnectionIndicator readError={readError} activityError={activityError} loading={readLoading} onRetry={onRetryRead} />
        {!collapsed && (
          <div className={styles.brandActions}>
            <DropdownMenu placement="below" button={{
              label: 'Sidebar view options', icon: <Icon name="more" />,
              isIconOnly: true, variant: 'ghost', size: 'sm', className: styles.iconButton,
            }}>
              <DropdownMenuSubMenu label="Hidden groups" isDisabled={hiddenGroups.length === 0}>
                {hiddenGroups.map((group) => <DropdownMenuItem key={group.id} label={`Show ${group.title}`} onClick={group.restore} />)}
              </DropdownMenuSubMenu>
            </DropdownMenu>
            <button
              type="button"
              data-nc-role="icon"
              className={styles.iconButton}
              aria-label="Collapse sidebar"
              aria-expanded="true"
              onClick={onToggleCollapsed}
            >
              <Icon name="chevron-left" />
            </button>
          </div>
        )}
      </div>
      <OperationFeedback feedback={writeFeedback} />

      {collapsed ? (
        <>
          {/* The rail's colours belong only to the indicator vocabulary and the current
                        location; titles never carry a state colour. */}
          {preferences.sidebarGroupVisible('waiting') && waiting.length > 0 && (
            <div className={styles.stripWaiting} aria-label={`${waiting.length} waiting on you`}>
              {waiting.length}
            </div>
          )}
          {/* An initial, not a colour chip: a letter says which area without spending the
                        channel the indicator vocabulary reserves for state. */}
          {(preferences.sidebarGroupVisible('areas') ? shownAreas : []).map((area) => (
            <button
              key={area.id}
              type="button"
              data-nc-role="row"
              className={styles.stripItem}
              aria-label={`Show area ${area.name}`}
              title={area.name}
              onClick={() => {
                pendingAreaFocusRef.current = area.id;
                preferences.setSidebarGroupExpanded('areas', true);
                preferences.setAreaExpanded(area.id, true);
                onToggleCollapsed();
              }}
            >
              <span className={styles.stripInitial} aria-hidden="true">{initialsOf(area.name)[0]}</span>
            </button>
          ))}
        </>
      ) : (
        <>
          {shownSections.map((id) => {
            if (id !== 'areas') {
              const group = groups.find((group) => group.id === id)!;
              return <SidebarTrackGroup key={id} title={group.title} label={group.title} level="section"
                management={management(id, `Group actions for ${group.title}`)}
                tracks={group.tracks} areas={userAreas} activeTrackId={activeTrackId} markCurrent={false}
                expanded={preferences.sidebarGroupExpanded(id)}
                onToggle={(value) => preferences.setSidebarGroupExpanded(id, value)} {...rowProps} />;
            }
            return <SidebarGroup key={id} title="Areas" label="Areas" level="section"
            management={management(id, 'Group actions for Areas')}
            expanded={areasExpanded}
            onToggle={(value) => preferences.setSidebarGroupExpanded('areas', value)}
            actions={<button type="button" data-nc-role="icon" className={`${styles.iconButton} ${styles.groupAction}`}
              aria-label="New area" onClick={onRequestCreateArea}><Icon name="plus" /></button>}>
            {readError === null && !readLoading && userAreas.length === 0 && (
              <button
                type="button"
                data-nc-role="row"
                className={styles.emptyAreaCreate}
                onClick={onRequestCreateArea}
              >
                Create your first area
              </button>
            )}

            {shownAreas.length > 0 && (
              <div className={styles.areaGroups}>
                {shownAreas.map((area) => (
                  <AreaGroup
                    key={area.id}
                    area={area}
                    management={management(`area:${area.id}`, `Area actions for ${area.name}`)}
                    areaTracks={tracksByArea.get(area.id) ?? []}
                    activeTrackId={activeTrackId}
                    expanded={preferences.areaExpanded(area.id)}
                    onToggle={(nextExpanded) => preferences.setAreaExpanded(area.id, nextExpanded)}
                    showClosed={preferences.areaShowsClosed(area.id)}
                    onSetShowClosed={(next) => preferences.setAreaShowsClosed(area.id, next)}
                    disclosureRef={(element) => {
                      if (element === null) areaDisclosureRefs.current.delete(area.id);
                      else areaDisclosureRefs.current.set(area.id, element);
                    }}
                    onEdit={() => onRequestEditArea(area)}
                    onRequestDelete={areaConfirm.request}
                    onNewTrack={onNewTrack}
                    {...rowProps}
                  />
                ))}
              </div>
            )}
          </SidebarGroup>;
          })}

        </>
      )}

      <div className={styles.userRow}>
            <Menu
              items={[
                { label: 'Settings', onSelect: onOpenSettings },
                { label: 'Plugins', onSelect: onOpenPlugins },
                { label: 'Sign out', onSelect: onSignOut },
              ]}
              wrapClassName={styles.menuWrap}
              menuClassName={styles.menu}
              itemClassName={styles.menuItem}
              trigger={(triggerProps) => (
                <button
                  {...triggerProps}
                  type="button"
                  data-nc-role="icon"
                  className={styles.avatar}
                  aria-label={`Account menu for ${userLabel}`}
                >
                  {initialsOf(userLabel)}
                </button>
              )}
            />
      </div>

      <ConfirmDialog
        open={trackConfirm.open}
        title={DELETE_TRACK_COPY.title}
        description={DELETE_TRACK_COPY.description}
        confirmLabel={DELETE_TRACK_COPY.confirmLabel}
        confirmBusyLabel="Deleting…"
        confirmState={trackConfirm.pending ? 'busy' : 'ready'}
        onConfirm={trackConfirm.confirm}
        onCancel={trackConfirm.cancel}
      />
      <OperationFeedback feedback={trackConfirm.feedback} />
      <OperationFeedback feedback={areaConfirm.feedback} />
      {/* Deleting an area cascades to every track inside it: the one operation that
                earns a typed confirm. */}
      <ConfirmDialog
        open={areaConfirm.open}
        title={areaCopy.title}
        description={<TypedDeleteBody
          copy={areaCopy}
          expected={deletingArea?.name ?? ''}
          value={typed.value}
          inputRef={typed.inputRef}
          onChange={typed.setValue}
        />}
        confirmLabel={areaCopy.confirmLabel}
        confirmBusyLabel="Deleting…"
        confirmState={areaConfirm.pending ? 'busy' : (typed.matches ? 'ready' : 'blocked')}
        initialFocusRef={typed.inputRef}
        onConfirm={areaConfirm.confirm}
        onCancel={areaConfirm.cancel}
      />
    </nav>
  );
}
