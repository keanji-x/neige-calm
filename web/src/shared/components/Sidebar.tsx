import { useCallback, useEffect, useMemo, useRef } from 'react';
import { useState } from '../state';
import { Menu, type MenuItem } from '../../ui/Menu/Menu';
import { useSession } from '../../app/SessionProvider';
import type { Area, Route, Track } from '../../types';
import { isRunning, sortByLifecycleRank, trackNeedsUserAttention } from '../lifecycle';
import { trackDisplayTitle } from '../trackTitle';
import { ConfirmDialog } from '../../ui/ConfirmDialog/ConfirmDialog';
import { ChevronIcon } from './ChevronIcon';
import { CloseIcon } from './CloseIcon';
import { PinIcon } from './PinIcon';
import { PlusIcon } from './PlusIcon';

// ---------------- Sidebar ----------------

const EXPANDED_AREAS_STORAGE_KEY = 'calm:sidebar:expandedAreas';
const SIDEBAR_COLLAPSED_STORAGE_KEY = 'calm:sidebar:collapsed';

type ExpandedAreas = Record<string, true>;

function readSidebarCollapsed(): boolean {
  if (typeof window === 'undefined') return false;
  try {
    return window.localStorage.getItem(SIDEBAR_COLLAPSED_STORAGE_KEY) === 'true';
  } catch {
    return false;
  }
}

function writeSidebarCollapsed(collapsed: boolean) {
  if (typeof window === 'undefined') return;
  try {
    window.localStorage.setItem(
      SIDEBAR_COLLAPSED_STORAGE_KEY,
      collapsed ? 'true' : 'false',
    );
  } catch {
    // localStorage may throw in private browsing or under quota pressure.
  }
}

function readExpandedAreas(): ExpandedAreas {
  if (typeof window === 'undefined') return {};
  try {
    const raw = window.localStorage.getItem(EXPANDED_AREAS_STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      return {};
    }
    const expanded: ExpandedAreas = {};
    for (const [areaId, value] of Object.entries(parsed)) {
      if (value === true) expanded[areaId] = true;
    }
    return expanded;
  } catch {
    return {};
  }
}

function writeExpandedAreas(expanded: ExpandedAreas) {
  if (typeof window === 'undefined') return;
  try {
    window.localStorage.setItem(
      EXPANDED_AREAS_STORAGE_KEY,
      JSON.stringify(expanded),
    );
  } catch {
    // localStorage may throw in private browsing or under quota pressure.
  }
}

function useExpandedAreas(): [
  ExpandedAreas,
  (areaId: string) => void,
  (areaId: string) => void,
] {
  const [expandedAreas, setExpandedAreas] = useState<ExpandedAreas>(
    () => readExpandedAreas(),
  );
  const toggleAreaExpanded = useCallback((areaId: string) => {
    setExpandedAreas((current) => {
      const next: ExpandedAreas = { ...current };
      if (next[areaId]) {
        delete next[areaId];
      } else {
        next[areaId] = true;
      }
      writeExpandedAreas(next);
      return next;
    });
  }, [setExpandedAreas]);
  const expandArea = useCallback((areaId: string) => {
    setExpandedAreas((current) => {
      if (current[areaId]) return current;
      const next: ExpandedAreas = { ...current, [areaId]: true };
      writeExpandedAreas(next);
      return next;
    });
  }, [setExpandedAreas]);
  return [expandedAreas, toggleAreaExpanded, expandArea];
}

function areaTracksListId(areaId: string): string {
  return `sidebar-area-tracks-${encodeURIComponent(areaId)}`;
}

export function Sidebar({
  areas,
  tracks,
  route,
  onGo,
  onCreateArea,
  onDeleteArea,
  onDeleteTrack,
  onPinTrack,
  onOpenSettings,
  onSignOut,
}: {
  areas: Area[];
  tracks: Track[];
  route: Route;
  onGo: (r: Route) => void;
  /** Bootstrap affordance: renders a small `+` icon button on the Areas
   *  section header that expands an inline name input at the top of the
   *  area list. Lives here (not in AreaPage) because creating the *first*
   *  area has no other home. Track creation, by contrast, lives inside
   *  AreaPage where the area context is already established. */
  onCreateArea?: (name: string, color: string) => void | Promise<void>;
  /** Per-row delete on each area. When provided, every area row reveals a
   *  hover `×` that opens a single shared ConfirmDialog. Mirrors the
   *  TrackRow delete pattern. Optional so tests can render the sidebar
   *  without wiring deletion. */
  onDeleteArea?: (areaId: string) => void | Promise<void>;
  /** Per-row delete on each track. When provided, every track row reveals a
   *  hover `×` that opens a single shared ConfirmDialog. */
  onDeleteTrack?: (trackId: string) => void | Promise<void>;
  /** Pin or unpin a track. Optional so tests / sub-trees that render the
   *  sidebar without a mutation hook don't have to wire it up. When
   *  provided, every track row renders a hover-revealed pin button. */
  onPinTrack?: (trackId: string, pin: boolean) => void | Promise<void>;
  /** Open the app-global settings page. Optional so tests / sub-trees that
   *  render the sidebar without a router don't have to wire it up. */
  onOpenSettings?: () => void;
  /** Sign the current user out. Optional so tests / sub-trees that render
   *  the sidebar without a router don't have to wire it up. */
  onSignOut?: () => void;
}) {
  // Single shared ConfirmDialog at the sidebar root; `pendingDelete`
  // carries the area being confirmed so the dialog text reflects the
  // actual area name. Mirrors Area.tsx's `pendingDeleteTrack` pattern.
  const [pendingDelete, setPendingDelete] = useState<Area | null>(null);
  const [pendingDeleteTrack, setPendingDeleteTrack] = useState<Track | null>(null);
  const [activeTrackRowEl, setActiveTrackRowEl] = useState<HTMLDivElement | null>(
    null,
  );
  const [sidebarCollapsed, setSidebarCollapsed] = useState(
    () => readSidebarCollapsed(),
  );
  const [expandedAreas, toggleAreaExpanded, expandArea] = useExpandedAreas();
  const activeTrackId = route.name === 'track' ? route.id : null;
  const activeAreaId = useMemo(
    () => (
      activeTrackId
        ? tracks.find((w) => w.id === activeTrackId)?.areaId ?? null
        : null
    ),
    [activeTrackId, tracks],
  );
  const setActiveTrackRowRef = useCallback(
    (node: HTMLDivElement | null) => {
      setActiveTrackRowEl(node);
    },
    [setActiveTrackRowEl],
  );
  useEffect(() => {
    if (activeAreaId == null) return;
    expandArea(activeAreaId);
  }, [activeTrackId, activeAreaId, expandArea]);
  useEffect(() => {
    activeTrackRowEl?.scrollIntoView?.({
      block: 'nearest',
      behavior: 'smooth',
    });
  }, [activeTrackId, activeTrackRowEl]);
  const cancelDelete = () => setPendingDelete(null);
  const confirmDelete = async () => {
    const c = pendingDelete;
    setPendingDelete(null);
    if (!c || !onDeleteArea) return;
    await onDeleteArea(c.id);
  };
  const openDeleteTrackDialog = (w: Track) => {
    if (!onDeleteTrack) return;
    setPendingDeleteTrack(w);
  };
  const cancelDeleteTrack = () => setPendingDeleteTrack(null);
  const confirmDeleteTrack = () => {
    const w = pendingDeleteTrack;
    setPendingDeleteTrack(null);
    if (!w || !onDeleteTrack) return;
    void onDeleteTrack(w.id);
  };
  const toggleSidebarCollapsed = () => {
    setSidebarCollapsed((current) => {
      const next = !current;
      writeSidebarCollapsed(next);
      return next;
    });
  };
  // Pinned tracks sorted by the timestamp they were pinned, oldest first
  // so the order is stable and user-determined (first pin = top).
  const pinnedTracks = tracks
    .filter((w) => w.pinnedAt != null)
    .sort((a, b) => a.pinnedAt! - b.pinnedAt!);
  // Issue #254 — OR'd predicate: lifecycle ∪ kernel-card-FSM. Catches
  // both "Planner Agent said blocked/reviewing/failed" AND "a worker card
  // hit an AwaitingInput/Errored hook before Planner Agent could drive
  // lifecycle". The latter is the regression hole #248's deletion of
  // the track-level FSM union left open.
  // Waiting includes pinned attention tracks intentionally: a pinned
  // waiting track appears in both Pinned and Waiting on you.
  const waitingTracks = tracks.filter(trackNeedsUserAttention);
  // Sub-landmarks inside the outer <aside aria-label="Navigation">:
  //   <nav aria-label="Sidebar navigation">  → Today button
  //   <section aria-label="Waiting on you">  → side-track rows (when any)
  //   <section aria-label="Pinned">          → pinned track rows (when any)
  //   <nav aria-label="Areas">               → area-nav buttons + New area
  // Two <nav>s rather than one because the "Waiting on you" section sits
  // visually between Today and the area list and reads as a third
  // concern (tracks needing attention) — folding the area list into the
  // top nav would either reorder the DOM or nest the section inside a
  // nav. Both landmarks have unique accessible names so the
  // `landmark-unique` axe rule stays green.
  //
  const collapseToggle = (
    <button
      type="button"
      className="side-collapse-toggle"
      onClick={toggleSidebarCollapsed}
      aria-expanded={!sidebarCollapsed}
      aria-label={sidebarCollapsed ? 'Expand sidebar' : 'Collapse sidebar'}
      title={sidebarCollapsed ? 'Expand sidebar' : 'Collapse sidebar'}
    >
      <ChevronIcon />
    </button>
  );

  return (
    <aside
      className={'side' + (sidebarCollapsed ? ' side--collapsed' : '')}
      aria-label="Navigation"
    >
      <div className="side-today-row">
        {!sidebarCollapsed && (
          <nav className="side-nav" aria-label="Sidebar navigation">
            <button
              className={'nav-item nav-today' + (route.name === 'today' ? ' active' : '')}
              onClick={() => onGo({ name: 'today' })}
            >
              <span className="lbl">Today</span>
            </button>
          </nav>
        )}
        {collapseToggle}
      </div>

      {!sidebarCollapsed && (
        <>
          {waitingTracks.length > 0 && (
            <section className="side-section attn-zone" aria-label="Waiting on you">
              <div className="nav-label warn-text">Waiting on you</div>
              {waitingTracks.map((w) => {
                const area = areas.find((c) => c.id === w.areaId);
                const active = route.name === 'track' && route.id === w.id;
                const displayTitle = trackDisplayTitle(w.title);
                return (
                  <TrackRow
                    key={w.id}
                    track={w}
                    active={active}
                    area={area ?? null}
                    title={area ? `${area.name} · ${displayTitle}` : displayTitle}
                    onGo={() => onGo({ name: 'track', id: w.id })}
                    onPinTrack={onPinTrack}
                    onDeleteTrack={onDeleteTrack ? openDeleteTrackDialog : undefined}
                    rowRef={active ? setActiveTrackRowRef : undefined}
                  />
                );
              })}
            </section>
          )}

          {pinnedTracks.length > 0 && (
            <section className="side-section" aria-label="Pinned">
              <div className="nav-label">Pinned</div>
              {pinnedTracks.map((w) => {
                const area = areas.find((c) => c.id === w.areaId);
                const active = route.name === 'track' && route.id === w.id;
                const displayTitle = trackDisplayTitle(w.title);
                return (
                  <TrackRow
                    key={w.id}
                    track={w}
                    active={active}
                    area={area ?? null}
                    title={area ? `${area.name} · ${displayTitle}` : displayTitle}
                    onGo={() => onGo({ name: 'track', id: w.id })}
                    onPinTrack={onPinTrack}
                    onDeleteTrack={onDeleteTrack ? openDeleteTrackDialog : undefined}
                    rowRef={active ? setActiveTrackRowRef : undefined}
                  />
                );
              })}
            </section>
          )}

          <nav className="side-nav side-areas" aria-label="Areas">
            <AreasHeader onCreate={onCreateArea} />
            {areas.map((area) => {
              const cw = tracks.filter((w) => w.areaId === area.id);
              // Pinned tracks intentionally appear in both the quick-access
              // Pinned section and their area's inline list; pinning is not
              // relocation, and the track still belongs to this area.
              const inlineTracks = sortByLifecycleRank(cw);
              const running = cw.filter((w) => isRunning(w.lifecycle)).length;
              // Match the top-of-sidebar "Waiting on you" predicate, including
              // pinned attention tracks, so area warn badges surface pinned work.
              const waiting = cw.filter(trackNeedsUserAttention).length;
              const active = route.name === 'area' && route.areaId === area.id;
              const expanded = !!expandedAreas[area.id];
              const listId = areaTracksListId(area.id);
              const showInlineTracks = expanded && inlineTracks.length > 0;
              // Single right-edge badge slot: warn-red waiting count beats
              // muted total count; empty when there are no tracks at all.
              const badge =
                waiting > 0
                  ? { kind: 'warn' as const, n: waiting }
                  : cw.length > 0
                    ? { kind: 'muted' as const, n: cw.length }
                    : null;
              return (
                <div
                  key={area.id}
                  className="area-block"
                  style={{ '--area-color': area.color } as React.CSSProperties}
                >
                  <div className="area-row" role="group">
                    <button
                      type="button"
                      className={'area-row-chevron' + (expanded ? ' expanded' : '')}
                      onClick={(e) => {
                        e.stopPropagation();
                        toggleAreaExpanded(area.id);
                      }}
                      aria-expanded={expanded}
                      aria-controls={showInlineTracks ? listId : undefined}
                      aria-label={`${expanded ? 'Collapse' : 'Expand'} area ${area.name}`}
                    >
                      <ChevronIcon />
                    </button>
                    <button
                      className={'area-nav' + (active ? ' active' : '')}
                      onClick={() => onGo({ name: 'area', areaId: area.id })}
                    >
                      <span className="swatch-wrap">
                        <span
                          className={'swatch' + (running > 0 ? ' pulse' : '')}
                          style={{ background: area.color }}
                        />
                      </span>
                      <span className="lbl">{area.name}</span>
                      {badge && (
                        <span
                          className={'area-nav-badge ' + badge.kind}
                          aria-hidden="true"
                        >
                          {badge.n}
                        </span>
                      )}
                    </button>
                    {onDeleteArea && (
                      <button
                        type="button"
                        className="area-row-delete"
                        onClick={(e) => {
                          e.stopPropagation();
                          setPendingDelete(area);
                        }}
                        title={`Delete area "${area.name}"`}
                        aria-label={`Delete area "${area.name}"`}
                      >
                        <CloseIcon />
                      </button>
                    )}
                  </div>
                  {showInlineTracks && (
                    <div
                      id={listId}
                      className="side-areas-tracks"
                      role="group"
                      aria-label={`Tracks in ${area.name}`}
                    >
                      {inlineTracks.map((w) => {
                        const trackActive = route.name === 'track' && route.id === w.id;
                        const displayTitle = trackDisplayTitle(w.title);
                        return (
                          <TrackRow
                            key={w.id}
                            track={w}
                            active={trackActive}
                            area={null}
                            title={displayTitle}
                            onGo={() => onGo({ name: 'track', id: w.id })}
                            onPinTrack={onPinTrack}
                            onDeleteTrack={onDeleteTrack ? openDeleteTrackDialog : undefined}
                            rowRef={trackActive ? setActiveTrackRowRef : undefined}
                          />
                        );
                      })}
                    </div>
                  )}
                </div>
              );
            })}
          </nav>

          <UserMenu onOpenSettings={onOpenSettings} onSignOut={onSignOut} />

          <ConfirmDialog
            open={pendingDelete !== null}
            title="Delete area?"
            description={
              pendingDelete
                ? `Delete area "${pendingDelete.name}"? Its tracks and cards go too. This cannot be undone.`
                : null
            }
            confirmLabel="Delete area"
            cancelLabel="Cancel"
            onConfirm={confirmDelete}
            onCancel={cancelDelete}
          />
          <ConfirmDialog
            open={pendingDeleteTrack !== null}
            title="Delete track?"
            description={
              pendingDeleteTrack
                ? `Delete track "${trackDisplayTitle(pendingDeleteTrack.title)}"? Its cards (including any terminals) go too. This cannot be undone.`
                : null
            }
            confirmLabel="Delete track"
            cancelLabel="Cancel"
            onConfirm={confirmDeleteTrack}
            onCancel={cancelDeleteTrack}
          />
        </>
      )}
    </aside>
  );
}

// ---------------- UserMenu ----------------
//
// The Sidebar's avatar row is the single user-menu trigger. Clicking it
// (or pressing Enter/Space) opens a small popover anchored above with
// Settings + Sign out items. Both callbacks are optional so the Sidebar
// can be rendered without a router (e.g. in component tests); items
// referencing a missing handler are simply no-ops.
function UserMenu({
  onOpenSettings,
  onSignOut,
}: {
  onOpenSettings?: () => void;
  onSignOut?: () => void;
}) {
  const { displayName } = useSession();
  const initials = computeInitials(displayName);
  const items: MenuItem[] = [
    { label: 'Settings', onSelect: () => onOpenSettings?.() },
    { label: 'Sign out', onSelect: () => onSignOut?.() },
  ];
  return (
    <Menu
      items={items}
      wrapClassName="me-menu"
      menuClassName="me-menu-popover"
      itemClassName="me-menu-item"
      trigger={({
        ref,
        onClick,
        'aria-haspopup': ariaHasPopup,
        'aria-expanded': ariaExpanded,
      }) => (
        <button
          ref={ref}
          type="button"
          className="me-row me-trigger"
          onClick={onClick}
          aria-haspopup={ariaHasPopup}
          aria-expanded={ariaExpanded}
          aria-label="Open user menu"
        >
          <span className="me">{initials}</span>
          <span className="who">{displayName}</span>
        </button>
      )}
    />
  );
}

// First letter of each whitespace-separated word, upper-cased, capped at
// two chars. Falls back to the first two chars of the raw name when the
// display name has no whitespace (e.g. a single handle like "yuki").
function computeInitials(displayName: string): string {
  const trimmed = displayName.trim();
  if (!trimmed) return '';
  const words = trimmed.split(/\s+/);
  if (words.length === 1) {
    return trimmed.slice(0, 2).toUpperCase();
  }
  return words
    .slice(0, 2)
    .map((w) => w.charAt(0).toUpperCase())
    .join('');
}

// ---------------- AreasHeader ----------------
//
// Renders the "Areas" section label with a tiny `+` icon button anchored
// on the right edge of the same row. Clicking `+` expands an inline name
// input directly below the header (still at the top of the area list),
// so the trigger stays in view even when the area list overflows.

const PALETTE = ['#5a9', '#c97', '#79c', '#b86', '#6a8', '#a6c'];

function AreasHeader({
  onCreate,
}: {
  onCreate?: (name: string, color: string) => void | Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState('');
  const inputRef = useRef<HTMLInputElement | null>(null);

  if (!onCreate) {
    return <div className="nav-label">Areas</div>;
  }

  const openForm = () => {
    setOpen(true);
    queueMicrotask(() => inputRef.current?.focus());
  };
  const close = () => {
    setOpen(false);
    setName('');
  };
  const submit = async () => {
    const trimmed = name.trim();
    if (!trimmed) {
      close();
      return;
    }
    const color = PALETTE[Math.floor(Math.random() * PALETTE.length)];
    await onCreate(trimmed, color);
    close();
  };

  return (
    <>
      <div className="nav-label nav-label-row">
        <span>Areas</span>
        <button
          type="button"
          className="nav-label-add"
          onClick={openForm}
          title="New area"
          aria-label="New area"
        >
          <PlusIcon />
        </button>
      </div>
      {open && (
        <div className="area-nav-edit">
          <span className="swatch-wrap">
            <span className="swatch-plus">+</span>
          </span>
          <input
            ref={inputRef}
            value={name}
            onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === 'Enter') void submit();
              else if (e.key === 'Escape') close();
            }}
            onBlur={() => void submit()}
            placeholder="Name…"
          />
        </div>
      )}
    </>
  );
}

// ---------------- TrackRow ----------------
//
// A single track entry in the Pinned, Waiting-on-you, or inline area list.
// Rendered as `<div role="group">` containing sibling `<button>`s to
// avoid nested-button a11y violations: pin, navigation, and delete.
// The pin button is hover-revealed but always visible when the track is
// already pinned so unpin is discoverable on touch.

function TrackRow({
  track,
  active,
  area,
  title,
  onGo,
  onPinTrack,
  onDeleteTrack,
  rowRef,
}: {
  track: Track;
  active: boolean;
  area: { id: string; name: string } | null;
  title: string;
  onGo: () => void;
  onPinTrack?: (trackId: string, pin: boolean) => void | Promise<void>;
  onDeleteTrack?: (track: Track) => void;
  rowRef?: (node: HTMLDivElement | null) => void;
}) {
  const pinned = track.pinnedAt != null;
  const attention = trackNeedsUserAttention(track);
  const displayTitle = trackDisplayTitle(track.title);
  return (
    <div
      ref={rowRef}
      className={'side-track-row' + (active ? ' active' : '') + (attention ? ' attention' : '')}
      role="group"
    >
      {onPinTrack && (
        <button
          type="button"
          className={'side-track-pin' + (pinned ? ' pinned' : '')}
          onClick={(e) => {
            e.stopPropagation();
            void onPinTrack(track.id, !pinned);
          }}
          aria-label={pinned ? 'Unpin track' : 'Pin track'}
        >
          <PinIcon down={pinned} />
        </button>
      )}
      <button
        className={'side-track' + (active ? ' active' : '')}
        onClick={onGo}
        title={title}
      >
        <span className="side-track-title">{displayTitle}</span>
        {area && <span className="side-track-area">{area.name}</span>}
      </button>
      {onDeleteTrack && (
        <button
          type="button"
          className="side-track-delete"
          onClick={(e) => {
            e.stopPropagation();
            onDeleteTrack(track);
          }}
          title={`Delete track "${displayTitle}"`}
          aria-label={`Delete track "${displayTitle}"`}
        >
          <CloseIcon />
        </button>
      )}
    </div>
  );
}
