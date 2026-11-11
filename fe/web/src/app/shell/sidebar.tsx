import type { DialogOrigin } from '../../ui/dialog/public.tsx';
import type { SidebarTrackGroups } from './sidebar-track-groups.ts';
import { EmptyState } from '../../ui/empty-state/public.tsx';
// The workspace rail: shared groups with owner-defined membership and actions.
import { useEffect, useRef } from 'react';
import { DropdownMenu, DropdownMenuItem, DropdownMenuSubMenu } from '@astryxdesign/core/DropdownMenu';

import { visibleAreas, type Area } from '../../../../core/domain/area.ts';
import { DELETE_FAILURES, DELETE_TEXT, writeFailureText } from '../../../../core/domain/failure-class.ts';
import {
  TRACK_PATCH_FAILURES, TRACK_PATCH_TEXT, userVisibleTracks, type Track,
} from '../../../../core/domain/track.ts';
import { deleteAreaCopy, DELETE_TRACK_COPY } from '../../ui/confirm-dialog/copy.ts';
import { Icon } from '../../ui/icon/public.tsx';
import { ListText } from '../../ui/list-typography/public.tsx';
import { Menu } from '../../ui/menu/public.tsx';
import { ConnectionIndicator } from './connection-indicator.tsx';
import {
  OperationFeedback, DeleteConfirmDialog, useDeleteConfirm, useOperationFeedback,
} from '../../ui/operation-feedback/public.tsx';
import { useUiPreferences } from '../providers/ui-preferences.tsx';
import { useReadReceipts } from '../providers/read-receipts.tsx';
import { MARK_UNREAD_TEXT, READ_RECEIPT_FAILURES } from '../../../../core/domain/read-receipts.ts';
import { TypedDeleteBody, useTypedConfirm } from '../../ui/typed-confirm/public.tsx';
import type { NavTarget } from '../router/navigation.ts';
import { routeParamFromPath } from '../router/navigation.ts';
import { SIDEBAR_SECTION_IDS, moveSidebarGroup, type SidebarGroupId, type SidebarMove, type SidebarSectionId } from '../../../../core/view/sidebar-layout.ts';
import { AreaGroup } from './area-group.tsx';
import { SidebarTrackGroup, type GroupManagement } from './sidebar-group.tsx';
import styles from './shell.module.css';
import { navigationRowClass as rowLayout, navigationRowPresentation } from '../../ui/navigation-row/public.ts';

export type SidebarProps = Readonly<{
  areas: readonly Area[];
  tracksByArea: ReadonlyMap<string, readonly Track[]>;
  tracks: readonly Track[];
  /** The shell's one membership projection, shared with mobile shortcuts. */
  trackGroups: SidebarTrackGroups;
  currentPath: string;
  onGo: (target: NavTarget) => void;
  onRequestCreateArea: (origin: DialogOrigin) => void;
  onRequestEditArea: (area: Area, origin: DialogOrigin) => void;
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
  areas, tracksByArea, tracks, trackGroups: memberships, currentPath, onGo,
  onRequestCreateArea, onRequestEditArea, onDeleteArea, onNewTrack, onSetPinned, onDeleteTrack,
  onOpenSettings, onOpenPlugins, onSignOut, collapsed, onToggleCollapsed,
  userLabel = 'You', nowMs, readError = null, activityError = null,
  readLoading = false, onRetryRead = () => undefined,
}: SidebarProps) {
  const preferences = useUiPreferences();
  const createRef = useRef<HTMLSpanElement | null>(null);
  const railRef = useRef<HTMLElement | null>(null);
  const areaDisclosureRefs = useRef(new Map<string, HTMLButtonElement>());
  const shortcutDisclosureRefs = useRef(new Map<Exclude<SidebarSectionId, 'areas'>, HTMLButtonElement>());
  const groupTrackRefs = useRef(new Map<string, HTMLButtonElement>());
  const pendingAreaFocusRef = useRef<string | null>(null);
  const pendingOptionsFocusRef = useRef(false);
  const pendingPinFocusRef = useRef<Readonly<{
    from: Exclude<SidebarSectionId, 'areas'>;
    trackId: string;
    expectedPinned: boolean;
  }> | null>(null);
  const trackConfirm = useDeleteConfirm(onDeleteTrack, writeFailureText(DELETE_FAILURES, DELETE_TEXT));
  const areaConfirm = useDeleteConfirm(onDeleteArea, writeFailureText(DELETE_FAILURES, DELETE_TEXT), () => onGo({ name: 'today' }));
  const writeFeedback = useOperationFeedback();

  const userAreas = visibleAreas(areas);
  const userTracks = userVisibleTracks(tracks, areas);
  const receipts = useReadReceipts();
  const isUnread = (track: Track) => receipts.isUnread('track', track.id, track.activityAt);
  const waiting = memberships.waiting;
  const groups: ReadonlyArray<{ id: Exclude<SidebarSectionId, 'areas'>; title: string; icon: 'star' | 'pin'; tracks: readonly Track[] }> = [
    { id: 'focus', title: 'Focus', icon: 'star', tracks: memberships.focus },
    { id: 'pinned', title: 'Pinned', icon: 'pin', tracks: memberships.pinned },
  ];

  const sectionOrder = preferences.sidebarOrder('sections', SIDEBAR_SECTION_IDS);
  const sectionVisible = (id: SidebarSectionId) => preferences.sidebarGroupVisible(id)
    && (id === 'areas' || groups.find((group) => group.id === id)!.tracks.length > 0);
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
  const optionsButton = () => railRef.current?.querySelector<HTMLButtonElement>('[aria-label="Sidebar view options"]');
  useEffect(() => {
    if (!pendingOptionsFocusRef.current) return;
    pendingOptionsFocusRef.current = false;
    optionsButton()?.focus();
  });

  const activeTrackId = routeParamFromPath(currentPath, '/track/') ?? null;
  const primaryNavPresentation = navigationRowPresentation({ kind: 'entry', depth: 0 });
  const todayActive = currentPath === '/';
  const recipesActive = currentPath === '/recipes' || currentPath.startsWith('/recipes/');

  const deletingArea = userAreas.find((area) => area.id === areaConfirm.target);
  const typed = useTypedConfirm(deletingArea?.name ?? '');
  const areaCopy = deleteAreaCopy(
    deletingArea?.name ?? '',
    tracksByArea.get(areaConfirm.target ?? '')?.length,
  );

  // Reveal the current row when visible without undoing manual Area collapse.
  const activeAreaId = userTracks.find((track) => track.id === activeTrackId)?.areaId;
  const activeAreaExpanded = activeAreaId === undefined ? true : preferences.areaExpanded(activeAreaId);
  useEffect(() => {
    if (collapsed || activeTrackId === null) return;
    railRef.current?.querySelector('[aria-current="page"]')
      ?.scrollIntoView?.({ block: 'nearest' });
  }, [activeTrackId, collapsed, activeAreaExpanded]);

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

  /* Pinning from Focus or unpinning from Pinned removes that shortcut row (and its
     group when empty); transfer dropped focus to the Track's surviving row or group. */
  useEffect(() => {
    if (collapsed || pendingPinFocusRef.current === null) return;
    const pending = pendingPinFocusRef.current;
    const track = userTracks.find((item) => item.id === pending.trackId);
    if (track !== undefined && (track.pinnedAt !== null) !== pending.expectedPinned) return;
    pendingPinFocusRef.current = null;
    const active = document.activeElement;
    if (active !== null && active !== document.body && active !== document.documentElement) return;
    const targetShortcut = pending.from === 'focus' ? 'pinned' : 'focus';
    const target = (track === undefined ? undefined : (
      groupTrackRefs.current.get(`${targetShortcut}:${track.id}`)
      ?? groupTrackRefs.current.get(`area:${track.areaId}:${track.id}`)
      ?? shortcutDisclosureRefs.current.get(targetShortcut)
      ?? areaDisclosureRefs.current.get(track.areaId)
    ))
      ?? shortcutDisclosureRefs.current.get(pending.from)
      ?? optionsButton();
    target?.focus({ preventScroll: true });
    target?.scrollIntoView?.({ block: 'nearest' });
  });

  const setTrackRef = (groupKey: string, trackId: string, element: HTMLButtonElement | null) => {
    const key = `${groupKey}:${trackId}`;
    if (element === null) groupTrackRefs.current.delete(key);
    else groupTrackRefs.current.set(key, element);
  };
  const setPinnedFrom = (origin?: Exclude<SidebarSectionId, 'areas'>) => (trackId: string, next: boolean) => {
    const pending = (origin === 'focus' && next) || (origin === 'pinned' && !next)
      ? { from: origin, trackId, expectedPinned: next }
      : null;
    if (pending !== null) pendingPinFocusRef.current = pending;
    void writeFeedback.run(
      Promise.resolve(onSetPinned(trackId, next)),
      writeFailureText(TRACK_PATCH_FAILURES, TRACK_PATCH_TEXT.pin),
    ).then((ok) => {
      if (!ok && pending !== null && pendingPinFocusRef.current === pending) {
        pendingPinFocusRef.current = null;
      }
    });
  };
  const rowPropsFor = (groupKey: string, origin?: Exclude<SidebarSectionId, 'areas'>) => {
    const handleSetPinned = setPinnedFrom(origin);
    return {
      // The receipt compares the overlay's completion high-water mark, not `updatedAt`
      // (which moves on every rename and pin); `null` is never unread.
      isUnread,
      onGo,
      nowMs,
      onSetPinned: handleSetPinned,
      onDelete: trackConfirm.request,
      trackRowRef: (trackId: string, element: HTMLButtonElement | null) => setTrackRef(groupKey, trackId, element),
      trackActions: (track: Track) => ({
        areaPinned: preferences.areaTrackPinned(track.areaId, track.id),
        onSetPinned: handleSetPinned,
        onSetAreaPinned: (id: string, next: boolean) => preferences.setAreaTrackPinned(track.areaId, id, next),
        onMarkUnread: (id: string) => { void writeFeedback.run(receipts.markUnread('track', id), writeFailureText(READ_RECEIPT_FAILURES, MARK_UNREAD_TEXT)); },
      }),
    };
  };

  return (
    <nav ref={railRef} className={`${rowLayout('row')} ${styles.rail} ${collapsed ? styles.railCollapsed : ''}`} aria-label="Workspace">
      <div className={styles.brandRow}>
        {!collapsed && (
          <button
            type="button"
            data-nc-role="row"
            className={`${styles.brand} ${rowLayout('row')} ${rowLayout('labelStart')}`}
            aria-label="Go to Today"
            onClick={() => onGo({ name: 'today' })}
          >
            <span className={rowLayout('leading')}><span className={styles.brandMark} aria-hidden="true" /></span>
            <span className={styles.brandText}>Neige</span>
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
          <div className={`${styles.brandActions} ${rowLayout('inlineActions')}`}>
            <DropdownMenu placement="below" button={{
              label: 'Sidebar view options', icon: <Icon name="more" />,
              isIconOnly: true, variant: 'ghost', size: 'sm', className: styles.areaActionsButton,
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
              <Icon name="panel-left" />
            </button>
          </div>
        )}
      </div>
      <OperationFeedback feedback={writeFeedback} />

      {collapsed ? (
        <>
          {/* The rail's colours belong only to the indicator vocabulary and the current
                        location; titles never carry a state colour. */}
          {preferences.sidebarGroupVisible('focus') && waiting.length > 0 && (
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
          <div className={styles.primaryNav} data-nc-primary-nav="">
            <div className={`${styles.areaRowWrap} ${primaryNavPresentation.className}`}>
              <button
                type="button"
                data-nc-role="row"
                className={`${styles.areaRow} ${styles.primaryNavRow} ${todayActive ? styles.primaryNavActive : ''} ${rowLayout('content')}`}
                aria-current={todayActive ? 'page' : undefined}
                onClick={() => onGo({ name: 'today' })}
              >
                <span className={`${styles.primaryNavIcon} ${rowLayout('leading')}`} aria-hidden="true">
                  <Icon name="calendar" size="md" />
                </span>
                <ListText tone={primaryNavPresentation.tone} emphasis={todayActive ? 'selected' : 'medium'} className={styles.areaName} title="Today">Today</ListText>
              </button>
            </div>
            <div className={`${styles.areaRowWrap} ${primaryNavPresentation.className}`}>
              <button
                type="button"
                data-nc-role="row"
                className={`${styles.areaRow} ${styles.primaryNavRow} ${recipesActive ? styles.primaryNavActive : ''} ${rowLayout('content')}`}
                aria-current={recipesActive ? 'page' : undefined}
                onClick={() => onGo({ name: 'recipes' })}
              >
                <span className={`${styles.primaryNavIcon} ${rowLayout('leading')}`} aria-hidden="true">
                  <Icon name="tasks" size="md" />
                </span>
                <ListText tone={primaryNavPresentation.tone} emphasis={recipesActive ? 'selected' : 'medium'} className={styles.areaName} title="Recipes">Recipes</ListText>
              </button>
            </div>
          </div>
          <div className={styles.sidebarSections}>
          {shownSections.map((id) => {
            if (id !== 'areas') {
              const group = groups.find((group) => group.id === id)!;
              return <SidebarTrackGroup key={id} title={group.title} label={group.title} icon={group.icon} level="section"
                management={management(id, `Group actions for ${group.title}`)}
                emptyState={null} tracks={group.tracks} areas={userAreas} activeTrackId={activeTrackId} markCurrent={false}
                expanded={preferences.sidebarGroupExpanded(id)}
                onToggle={(value) => preferences.setSidebarGroupExpanded(id, value)}
                disclosureRef={(element) => {
                  if (element === null) shortcutDisclosureRefs.current.delete(id);
                  else shortcutDisclosureRefs.current.set(id, element);
                }}
                {...rowPropsFor(id, id)} />;
            }
            return <div key={id} className={styles.areaGroups}>
              {readError === null && !readLoading && userAreas.length === 0 && (
                <div className={styles.emptyArea}>
                  <EmptyState variant="compact" icon={<Icon name="folder" />} title="还没有 Area" description="项目与 Track 会按 Area 归类。" />
                </div>
              )}

              {shownAreas.map((area) => (
                <AreaGroup
                  key={area.id}
                  area={area}
                  management={management(`area:${area.id}`, `Area actions for ${area.name}`)}
                  readReady={readError === null && !readLoading && activityError === null}
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
                  onEdit={(origin) => onRequestEditArea(area, origin)}
                  onRequestDelete={areaConfirm.request}
                  onNewTrack={onNewTrack}
                  {...rowPropsFor(`area:${area.id}`)}
                />
              ))}

              <div className={`${styles.areaRowWrap} ${primaryNavPresentation.className}`}>
                <span ref={createRef} className={styles.newAreaAnchor}>
                  <button
                    type="button"
                    data-nc-role="row"
                    className={`${styles.areaRow} ${styles.newAreaRow} ${rowLayout('content')}`}
                    aria-label="New area"
                    onClick={() => onRequestCreateArea({ kind: 'anchor', anchor: { ref: createRef }, unavailable: 'surface-center' })}
                  >
                    <span className={`${styles.newAreaIcon} ${rowLayout('leading')}`} aria-hidden="true">
                      <Icon name="plus" size="md" />
                    </span>
                    <ListText tone={primaryNavPresentation.tone} className={styles.areaName} title="New area">New area</ListText>
                  </button>
                </span>
              </div>
            </div>;
          })}
          </div>

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
            {!collapsed && <span className={styles.userName} aria-hidden="true">{userLabel}</span>}
            {!collapsed && (
              <button
                type="button"
                data-nc-role="icon"
                className={`${styles.iconButton} ${styles.userSettingsButton}`}
                aria-label="Open settings"
                title="Settings"
                onClick={onOpenSettings}
              >
                <Icon name="settings" size="sm" />
              </button>
            )}
      </div>

      <DeleteConfirmDialog confirm={trackConfirm} copy={DELETE_TRACK_COPY} />
      {/* Deleting an area cascades to every track inside it: the one operation that
                earns a typed confirm. */}
      <DeleteConfirmDialog confirm={areaConfirm} copy={areaCopy} blocked={!typed.matches} initialFocusRef={typed.inputRef}
        body={<TypedDeleteBody copy={areaCopy} expected={deletingArea?.name ?? ''}
          value={typed.value} inputRef={typed.inputRef} onChange={typed.setValue} />} />
    </nav>
  );
}
