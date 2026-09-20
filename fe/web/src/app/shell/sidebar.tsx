// The workspace rail: three sections in a fixed order.

import { ListText } from '../../ui/list-typography/public.tsx';
import { useEffect, useRef } from 'react';
import { useCollapsible } from '@astryxdesign/core/Collapsible';
import { DropdownMenu, DropdownMenuItem } from '@astryxdesign/core/DropdownMenu';

import { areaOf, visibleAreas, type Area } from '../../../../core/domain/area.ts';
import { hasFailed, needsUserAttention, userVisibleTracks, visibleTracks, type Track } from '../../../../core/domain/track.ts';
import { TrackRow } from '../../features/track/row/public.tsx';
import { deleteAreaCopy, DELETE_TRACK_COPY } from '../../ui/confirm-dialog/copy.ts';
import { ConfirmDialog } from '../../ui/dialog/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { Menu } from '../../ui/menu/public.tsx';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import {
  OperationFeedback, useDeleteConfirm, useOperationFeedback,
} from '../../ui/operation-feedback/public.tsx';
import { useUiPreferences } from '../providers/ui-preferences.tsx';
import { TypedDeleteBody, useTypedConfirm } from '../../ui/typed-confirm/public.tsx';
import type { NavTarget } from '../router/navigation.ts';
import { routeParamFromPath } from '../router/navigation.ts';
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
  const trackConfirm = useDeleteConfirm(onDeleteTrack);
  const areaConfirm = useDeleteConfirm(onDeleteArea, () => onGo({ name: 'today' }));
  const writeFeedback = useOperationFeedback();

  const userAreas = visibleAreas(areas);
  const userTracks = userVisibleTracks(tracks, areas);
  // "Waiting on you" is what the kernel says needs a person: input or repair.
  const waiting = userTracks.filter((track) => needsUserAttention(track) || hasFailed(track));
  const pinned = userTracks.filter((track) => track.pinnedAt !== null)
    .toSorted((left, right) => (right.pinnedAt ?? 0) - (left.pinnedAt ?? 0));

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

  const rowProps = {
    currentPath,
    // The receipt compares the overlay's completion high-water mark, not `updatedAt`
    // (which moves on every rename and pin); `null` is never unread.
    isUnread: (track: Track) => preferences.isUnread('track', track.id, track.activityAt ?? 0),
    onGo,
    nowMs,
    onSetPinned: (trackId: string, next: boolean) => {
      void writeFeedback.run(Promise.resolve(onSetPinned(trackId, next)), 'Could not update the track.');
    },
    onDelete: trackConfirm.request,
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
        {collapsed ? (
          <button
            type="button"
            className={styles.iconButton}
            aria-label="Expand sidebar"
            aria-expanded="false"
            onClick={onToggleCollapsed}
          >
            <span className={styles.brandMark} aria-hidden="true" />
          </button>
        ) : (
          <button
            type="button"
            data-nc-role="icon"
            className={`${styles.iconButton} ${styles.spring}`}
            aria-label="Collapse sidebar"
            aria-expanded="true"
            onClick={onToggleCollapsed}
          >
            <Icon name="chevron-left" />
          </button>
        )}
      </div>
      {readError !== null && <ErrorBox message={readError} onRetry={onRetryRead} />}
      {activityError !== null && <ErrorBox message={`Track activity is unavailable: ${activityError}`} onRetry={onRetryRead} />}
      {readLoading && <div role="status">Loading workspace…</div>}
      <OperationFeedback feedback={writeFeedback} />

      {collapsed ? (
        <>
          {/* The rail's colours belong only to the indicator vocabulary and the current
                        location; titles never carry a state colour. */}
          {waiting.length > 0 && (
            <div className={styles.stripWaiting} aria-label={`${waiting.length} waiting on you`}>
              {waiting.length}
            </div>
          )}
          {/* An initial, not a colour chip: a letter says which area without spending the
                        channel the indicator vocabulary reserves for state. */}
          {userAreas.map((area) => (
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
          <TrackSection title="Waiting on you" tracks={waiting} areas={userAreas} {...rowProps} />
          <TrackSection title="Pinned" tracks={pinned} areas={userAreas} {...rowProps} />

          <div className={styles.section}>
            <div className={styles.sectionHead}>
              <ListText as="h2" tone="section" className={styles.sectionTitle}>Areas</ListText>
              <button
                type="button"
                data-nc-role="icon"
                className={styles.sectionAction}
                aria-label="New area"
                onClick={onRequestCreateArea}
              >
                <Icon name="plus" />
              </button>
            </div>

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

            {userAreas.length > 0 && (
              <div className={styles.areaGroups}>
                {userAreas.map((area) => (
                  <AreaGroup
                    key={area.id}
                    area={area}
                    areaTracks={visibleTracks(tracksByArea.get(area.id) ?? [])}
                    expanded={preferences.areaExpanded(area.id)}
                    onToggle={(nextExpanded) => preferences.setAreaExpanded(area.id, nextExpanded)}
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

type RowProps = Readonly<{
  isUnread: (track: Track) => boolean;
  currentPath: string;
  onGo: (target: NavTarget) => void;
  nowMs?: number;
  onSetPinned: (trackId: string, pinned: boolean) => void;
  onDelete: (trackId: string) => void;
}>;

/** The two shortcut sections: a section with no rows does not render at all, and their rows are never marked current. */
function TrackSection({ title, tracks, areas, onGo, nowMs, onSetPinned, onDelete, isUnread }: RowProps & {
  title: string;
  tracks: readonly Track[];
  areas: readonly Area[];
}) {
  if (tracks.length === 0) return null;
  return (
    <div className={styles.section}>
      <ListText as="h2" tone="section" className={styles.sectionTitle}>{title}</ListText>
      <div className={styles.sectionRows}>
        {tracks.map((track) => (
          <TrackRow
            key={track.id}
            track={track}
            unread={isUnread(track)}
            areaName={areaOf(track.areaId, areas)?.name}
            variant="rail"
            nowMs={nowMs}
            onOpen={(trackId) => onGo({ name: 'track', trackId })}
            onSetPinned={onSetPinned}
            onDelete={onDelete}
          />
        ))}
      </div>
    </div>
  );
}

/**
 * Navigation is `<button>` + `onGo`, never `<a href>`, the `+` included: this rail
 * does not mix the two activation models. The Area row is a disclosure, not
 * navigation. `+` and the actions menu are both permanently visible and never
 * share a slot.
 */
function AreaGroup({
  area, areaTracks, expanded, onToggle, disclosureRef, onEdit, onRequestDelete, onNewTrack,
  currentPath, onGo, nowMs, onSetPinned, onDelete, isUnread,
}: RowProps & {
  area: Area;
  areaTracks: readonly Track[];
  expanded: boolean;
  onToggle: (expanded: boolean) => void;
  disclosureRef: (element: HTMLButtonElement | null) => void;
  onEdit: () => void;
  onRequestDelete: (areaId: string) => void;
  onNewTrack: (areaId: string) => void;
}) {
  const disclosure = useCollapsible({
    isCollapsible: { isOpen: expanded, onOpenChange: onToggle },
  });
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
      {disclosure.isOpen && areaTracks.length > 0 && (
        <div className={styles.trackList}>
          {areaTracks.map((track) => (
            <TrackRow
              key={track.id}
              track={track}
            unread={isUnread(track)}
              variant="rail"
              nowMs={nowMs}
              active={routeParamFromPath(currentPath, '/track/') === track.id}
              onOpen={(trackId) => onGo({ name: 'track', trackId })}
              onSetPinned={onSetPinned}
              onDelete={onDelete}
            />
          ))}
        </div>
      )}
    </div>
  );
}
