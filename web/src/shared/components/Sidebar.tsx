import { useCallback, useEffect, useMemo, useRef } from 'react';
import { useState } from '../state';
import { Menu, type MenuItem } from '../../ui/Menu/Menu';
import { useSession } from '../../app/SessionProvider';
import type { Area, Route, Wave } from '../../types';
import { isRunning, sortByLifecycleRank, waveNeedsUserAttention } from '../lifecycle';
import { waveDisplayTitle } from '../waveTitle';
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

function areaWavesListId(areaId: string): string {
  return `sidebar-area-waves-${encodeURIComponent(areaId)}`;
}

export function Sidebar({
  areas,
  waves,
  route,
  onGo,
  onCreateArea,
  onDeleteArea,
  onDeleteWave,
  onPinWave,
  onOpenSettings,
  onSignOut,
}: {
  areas: Area[];
  waves: Wave[];
  route: Route;
  onGo: (r: Route) => void;
  /** Bootstrap affordance: renders a small `+` icon button on the Areas
   *  section header that expands an inline name input at the top of the
   *  area list. Lives here (not in AreaPage) because creating the *first*
   *  area has no other home. Wave creation, by contrast, lives inside
   *  AreaPage where the area context is already established. */
  onCreateArea?: (name: string, color: string) => void | Promise<void>;
  /** Per-row delete on each area. When provided, every area row reveals a
   *  hover `×` that opens a single shared ConfirmDialog. Mirrors the
   *  WaveRow delete pattern. Optional so tests can render the sidebar
   *  without wiring deletion. */
  onDeleteArea?: (areaId: string) => void | Promise<void>;
  /** Per-row delete on each wave. When provided, every wave row reveals a
   *  hover `×` that opens a single shared ConfirmDialog. */
  onDeleteWave?: (waveId: string) => void | Promise<void>;
  /** Pin or unpin a wave. Optional so tests / sub-trees that render the
   *  sidebar without a mutation hook don't have to wire it up. When
   *  provided, every wave row renders a hover-revealed pin button. */
  onPinWave?: (waveId: string, pin: boolean) => void | Promise<void>;
  /** Open the app-global settings page. Optional so tests / sub-trees that
   *  render the sidebar without a router don't have to wire it up. */
  onOpenSettings?: () => void;
  /** Sign the current user out. Optional so tests / sub-trees that render
   *  the sidebar without a router don't have to wire it up. */
  onSignOut?: () => void;
}) {
  // Single shared ConfirmDialog at the sidebar root; `pendingDelete`
  // carries the area being confirmed so the dialog text reflects the
  // actual area name. Mirrors Area.tsx's `pendingDeleteWave` pattern.
  const [pendingDelete, setPendingDelete] = useState<Area | null>(null);
  const [pendingDeleteWave, setPendingDeleteWave] = useState<Wave | null>(null);
  const [activeWaveRowEl, setActiveWaveRowEl] = useState<HTMLDivElement | null>(
    null,
  );
  const [sidebarCollapsed, setSidebarCollapsed] = useState(
    () => readSidebarCollapsed(),
  );
  const [expandedAreas, toggleAreaExpanded, expandArea] = useExpandedAreas();
  const activeWaveId = route.name === 'wave' ? route.id : null;
  const activeAreaId = useMemo(
    () => (
      activeWaveId
        ? waves.find((w) => w.id === activeWaveId)?.areaId ?? null
        : null
    ),
    [activeWaveId, waves],
  );
  const setActiveWaveRowRef = useCallback(
    (node: HTMLDivElement | null) => {
      setActiveWaveRowEl(node);
    },
    [setActiveWaveRowEl],
  );
  useEffect(() => {
    if (activeAreaId == null) return;
    expandArea(activeAreaId);
  }, [activeWaveId, activeAreaId, expandArea]);
  useEffect(() => {
    activeWaveRowEl?.scrollIntoView?.({
      block: 'nearest',
      behavior: 'smooth',
    });
  }, [activeWaveId, activeWaveRowEl]);
  const cancelDelete = () => setPendingDelete(null);
  const confirmDelete = async () => {
    const c = pendingDelete;
    setPendingDelete(null);
    if (!c || !onDeleteArea) return;
    await onDeleteArea(c.id);
  };
  const openDeleteWaveDialog = (w: Wave) => {
    if (!onDeleteWave) return;
    setPendingDeleteWave(w);
  };
  const cancelDeleteWave = () => setPendingDeleteWave(null);
  const confirmDeleteWave = () => {
    const w = pendingDeleteWave;
    setPendingDeleteWave(null);
    if (!w || !onDeleteWave) return;
    void onDeleteWave(w.id);
  };
  const toggleSidebarCollapsed = () => {
    setSidebarCollapsed((current) => {
      const next = !current;
      writeSidebarCollapsed(next);
      return next;
    });
  };
  // Pinned waves sorted by the timestamp they were pinned, oldest first
  // so the order is stable and user-determined (first pin = top).
  const pinnedWaves = waves
    .filter((w) => w.pinnedAt != null)
    .sort((a, b) => a.pinnedAt! - b.pinnedAt!);
  // Issue #254 — OR'd predicate: lifecycle ∪ kernel-card-FSM. Catches
  // both "Spec Agent said blocked/reviewing/failed" AND "a worker card
  // hit an AwaitingInput/Errored hook before Spec Agent could drive
  // lifecycle". The latter is the regression hole #248's deletion of
  // the wave-level FSM union left open.
  // Waiting includes pinned attention waves intentionally: a pinned
  // waiting wave appears in both Pinned and Waiting on you.
  const waitingWaves = waves.filter(waveNeedsUserAttention);
  // Sub-landmarks inside the outer <aside aria-label="Navigation">:
  //   <nav aria-label="Sidebar navigation">  → Today button
  //   <section aria-label="Waiting on you">  → side-wave rows (when any)
  //   <section aria-label="Pinned">          → pinned wave rows (when any)
  //   <nav aria-label="Areas">               → area-nav buttons + New area
  // Two <nav>s rather than one because the "Waiting on you" section sits
  // visually between Today and the area list and reads as a third
  // concern (waves needing attention) — folding the area list into the
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
          {waitingWaves.length > 0 && (
            <section className="side-section attn-zone" aria-label="Waiting on you">
              <div className="nav-label warn-text">Waiting on you</div>
              {waitingWaves.map((w) => {
                const area = areas.find((c) => c.id === w.areaId);
                const active = route.name === 'wave' && route.id === w.id;
                const displayTitle = waveDisplayTitle(w.title);
                return (
                  <WaveRow
                    key={w.id}
                    wave={w}
                    active={active}
                    area={area ?? null}
                    title={area ? `${area.name} · ${displayTitle}` : displayTitle}
                    onGo={() => onGo({ name: 'wave', id: w.id })}
                    onPinWave={onPinWave}
                    onDeleteWave={onDeleteWave ? openDeleteWaveDialog : undefined}
                    rowRef={active ? setActiveWaveRowRef : undefined}
                  />
                );
              })}
            </section>
          )}

          {pinnedWaves.length > 0 && (
            <section className="side-section" aria-label="Pinned">
              <div className="nav-label">Pinned</div>
              {pinnedWaves.map((w) => {
                const area = areas.find((c) => c.id === w.areaId);
                const active = route.name === 'wave' && route.id === w.id;
                const displayTitle = waveDisplayTitle(w.title);
                return (
                  <WaveRow
                    key={w.id}
                    wave={w}
                    active={active}
                    area={area ?? null}
                    title={area ? `${area.name} · ${displayTitle}` : displayTitle}
                    onGo={() => onGo({ name: 'wave', id: w.id })}
                    onPinWave={onPinWave}
                    onDeleteWave={onDeleteWave ? openDeleteWaveDialog : undefined}
                    rowRef={active ? setActiveWaveRowRef : undefined}
                  />
                );
              })}
            </section>
          )}

          <nav className="side-nav side-areas" aria-label="Areas">
            <AreasHeader onCreate={onCreateArea} />
            {areas.map((area) => {
              const cw = waves.filter((w) => w.areaId === area.id);
              // Pinned waves intentionally appear in both the quick-access
              // Pinned section and their area's inline list; pinning is not
              // relocation, and the wave still belongs to this area.
              const inlineWaves = sortByLifecycleRank(cw);
              const running = cw.filter((w) => isRunning(w.lifecycle)).length;
              // Match the top-of-sidebar "Waiting on you" predicate, including
              // pinned attention waves, so area warn badges surface pinned work.
              const waiting = cw.filter(waveNeedsUserAttention).length;
              const active = route.name === 'area' && route.areaId === area.id;
              const expanded = !!expandedAreas[area.id];
              const listId = areaWavesListId(area.id);
              const showInlineWaves = expanded && inlineWaves.length > 0;
              // Single right-edge badge slot: warn-red waiting count beats
              // muted total count; empty when there are no waves at all.
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
                      aria-controls={showInlineWaves ? listId : undefined}
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
                  {showInlineWaves && (
                    <div
                      id={listId}
                      className="side-areas-waves"
                      role="group"
                      aria-label={`Waves in ${area.name}`}
                    >
                      {inlineWaves.map((w) => {
                        const waveActive = route.name === 'wave' && route.id === w.id;
                        const displayTitle = waveDisplayTitle(w.title);
                        return (
                          <WaveRow
                            key={w.id}
                            wave={w}
                            active={waveActive}
                            area={null}
                            title={displayTitle}
                            onGo={() => onGo({ name: 'wave', id: w.id })}
                            onPinWave={onPinWave}
                            onDeleteWave={onDeleteWave ? openDeleteWaveDialog : undefined}
                            rowRef={waveActive ? setActiveWaveRowRef : undefined}
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
                ? `Delete area "${pendingDelete.name}"? Its waves and cards go too. This cannot be undone.`
                : null
            }
            confirmLabel="Delete area"
            cancelLabel="Cancel"
            onConfirm={confirmDelete}
            onCancel={cancelDelete}
          />
          <ConfirmDialog
            open={pendingDeleteWave !== null}
            title="Delete wave?"
            description={
              pendingDeleteWave
                ? `Delete wave "${waveDisplayTitle(pendingDeleteWave.title)}"? Its cards (including any terminals) go too. This cannot be undone.`
                : null
            }
            confirmLabel="Delete wave"
            cancelLabel="Cancel"
            onConfirm={confirmDeleteWave}
            onCancel={cancelDeleteWave}
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

// ---------------- WaveRow ----------------
//
// A single wave entry in the Pinned, Waiting-on-you, or inline area list.
// Rendered as `<div role="group">` containing sibling `<button>`s to
// avoid nested-button a11y violations: pin, navigation, and delete.
// The pin button is hover-revealed but always visible when the wave is
// already pinned so unpin is discoverable on touch.

function WaveRow({
  wave,
  active,
  area,
  title,
  onGo,
  onPinWave,
  onDeleteWave,
  rowRef,
}: {
  wave: Wave;
  active: boolean;
  area: { id: string; name: string } | null;
  title: string;
  onGo: () => void;
  onPinWave?: (waveId: string, pin: boolean) => void | Promise<void>;
  onDeleteWave?: (wave: Wave) => void;
  rowRef?: (node: HTMLDivElement | null) => void;
}) {
  const pinned = wave.pinnedAt != null;
  const attention = waveNeedsUserAttention(wave);
  const displayTitle = waveDisplayTitle(wave.title);
  return (
    <div
      ref={rowRef}
      className={'side-wave-row' + (active ? ' active' : '') + (attention ? ' attention' : '')}
      role="group"
    >
      {onPinWave && (
        <button
          type="button"
          className={'side-wave-pin' + (pinned ? ' pinned' : '')}
          onClick={(e) => {
            e.stopPropagation();
            void onPinWave(wave.id, !pinned);
          }}
          aria-label={pinned ? 'Unpin wave' : 'Pin wave'}
        >
          <PinIcon down={pinned} />
        </button>
      )}
      <button
        className={'side-wave' + (active ? ' active' : '')}
        onClick={onGo}
        title={title}
      >
        <span className="side-wave-title">{displayTitle}</span>
        {area && <span className="side-wave-area">{area.name}</span>}
      </button>
      {onDeleteWave && (
        <button
          type="button"
          className="side-wave-delete"
          onClick={(e) => {
            e.stopPropagation();
            onDeleteWave(wave);
          }}
          title={`Delete wave "${displayTitle}"`}
          aria-label={`Delete wave "${displayTitle}"`}
        >
          <CloseIcon />
        </button>
      )}
    </div>
  );
}
