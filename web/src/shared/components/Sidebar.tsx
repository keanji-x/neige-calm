import { Fragment, useCallback, useEffect, useMemo, useRef } from 'react';
import { useState } from '../state';
import { Menu, type MenuItem } from '../../ui/Menu/Menu';
import { useSession } from '../../app/SessionProvider';
import type { Cove, Route, Wave } from '../../types';
import { isRunning, sortByLifecycleRank, waveNeedsUserAttention } from '../lifecycle';
import { waveDisplayTitle } from '../waveTitle';
import { ConfirmDialog } from '../../ui/ConfirmDialog/ConfirmDialog';
import { ChevronIcon } from './ChevronIcon';
import { CloseIcon } from './CloseIcon';
import { PinIcon } from './PinIcon';
import { PlusIcon } from './PlusIcon';

// ---------------- Sidebar ----------------

const EXPANDED_COVES_STORAGE_KEY = 'calm:sidebar:expandedCoves';

type ExpandedCoves = Record<string, true>;

function readExpandedCoves(): ExpandedCoves {
  if (typeof window === 'undefined') return {};
  try {
    const raw = window.localStorage.getItem(EXPANDED_COVES_STORAGE_KEY);
    if (!raw) return {};
    const parsed: unknown = JSON.parse(raw);
    if (!parsed || typeof parsed !== 'object' || Array.isArray(parsed)) {
      return {};
    }
    const expanded: ExpandedCoves = {};
    for (const [coveId, value] of Object.entries(parsed)) {
      if (value === true) expanded[coveId] = true;
    }
    return expanded;
  } catch {
    return {};
  }
}

function writeExpandedCoves(expanded: ExpandedCoves) {
  if (typeof window === 'undefined') return;
  try {
    window.localStorage.setItem(
      EXPANDED_COVES_STORAGE_KEY,
      JSON.stringify(expanded),
    );
  } catch {
    // localStorage may throw in private browsing or under quota pressure.
  }
}

function useExpandedCoves(): [
  ExpandedCoves,
  (coveId: string) => void,
  (coveId: string) => void,
] {
  const [expandedCoves, setExpandedCoves] = useState<ExpandedCoves>(
    () => readExpandedCoves(),
  );
  const toggleCoveExpanded = useCallback((coveId: string) => {
    setExpandedCoves((current) => {
      const next: ExpandedCoves = { ...current };
      if (next[coveId]) {
        delete next[coveId];
      } else {
        next[coveId] = true;
      }
      writeExpandedCoves(next);
      return next;
    });
  }, [setExpandedCoves]);
  const expandCove = useCallback((coveId: string) => {
    setExpandedCoves((current) => {
      if (current[coveId]) return current;
      const next: ExpandedCoves = { ...current, [coveId]: true };
      writeExpandedCoves(next);
      return next;
    });
  }, [setExpandedCoves]);
  return [expandedCoves, toggleCoveExpanded, expandCove];
}

function coveWavesListId(coveId: string): string {
  return `sidebar-cove-waves-${encodeURIComponent(coveId)}`;
}

export function Sidebar({
  coves,
  waves,
  route,
  onGo,
  onCreateCove,
  onDeleteCove,
  onDeleteWave,
  onPinWave,
  onOpenSettings,
  onSignOut,
}: {
  coves: Cove[];
  waves: Wave[];
  route: Route;
  onGo: (r: Route) => void;
  /** Bootstrap affordance: renders a small `+` icon button on the Coves
   *  section header that expands an inline name input at the top of the
   *  cove list. Lives here (not in CovePage) because creating the *first*
   *  cove has no other home. Wave creation, by contrast, lives inside
   *  CovePage where the cove context is already established. */
  onCreateCove?: (name: string, color: string) => void | Promise<void>;
  /** Per-row delete on each cove. When provided, every cove row reveals a
   *  hover `×` that opens a single shared ConfirmDialog. Mirrors the
   *  WaveRow delete pattern. Optional so tests can render the sidebar
   *  without wiring deletion. */
  onDeleteCove?: (coveId: string) => void | Promise<void>;
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
  // carries the cove being confirmed so the dialog text reflects the
  // actual cove name. Mirrors Cove.tsx's `pendingDeleteWave` pattern.
  const [pendingDelete, setPendingDelete] = useState<Cove | null>(null);
  const [pendingDeleteWave, setPendingDeleteWave] = useState<Wave | null>(null);
  const [activeWaveRowEl, setActiveWaveRowEl] = useState<HTMLDivElement | null>(
    null,
  );
  const [expandedCoves, toggleCoveExpanded, expandCove] = useExpandedCoves();
  const activeWaveId = route.name === 'wave' ? route.id : null;
  const activeCoveId = useMemo(
    () => (
      activeWaveId
        ? waves.find((w) => w.id === activeWaveId)?.coveId ?? null
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
    if (activeCoveId == null) return;
    expandCove(activeCoveId);
  }, [activeWaveId, activeCoveId, expandCove]);
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
    if (!c || !onDeleteCove) return;
    await onDeleteCove(c.id);
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
  //   <nav aria-label="Coves">               → cove-nav buttons + New cove
  // Two <nav>s rather than one because the "Waiting on you" section sits
  // visually between Today and the cove list and reads as a third
  // concern (waves needing attention) — folding the cove list into the
  // top nav would either reorder the DOM or nest the section inside a
  // nav. Both landmarks have unique accessible names so the
  // `landmark-unique` axe rule stays green.
  //
  // Scoping role queries by these landmarks disambiguates buttons that
  // share an accessible name across sections — e.g. a wave titled
  // "Today" in the section vs. the Today nav button in the nav. See
  // docs/a11y-contract.md §2.2.
  return (
    <aside className="side" aria-label="Navigation">
      <nav className="side-nav" aria-label="Sidebar navigation">
        <button
          className={'nav-item nav-today' + (route.name === 'today' ? ' active' : '')}
          onClick={() => onGo({ name: 'today' })}
        >
          <span className="lbl">Today</span>
        </button>
      </nav>

      {waitingWaves.length > 0 && (
        <section className="side-section attn-zone" aria-label="Waiting on you">
          <div className="nav-label warn-text">Waiting on you</div>
          {waitingWaves.map((w) => {
            const cove = coves.find((c) => c.id === w.coveId);
            const active = route.name === 'wave' && route.id === w.id;
            const displayTitle = waveDisplayTitle(w.title);
            return (
              <WaveRow
                key={w.id}
                wave={w}
                active={active}
                cove={cove ?? null}
                title={cove ? `${cove.name} · ${displayTitle}` : displayTitle}
                onGo={() => onGo({ name: 'wave', id: w.id })}
                onPinWave={onPinWave}
                onDeleteWave={openDeleteWaveDialog}
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
            const cove = coves.find((c) => c.id === w.coveId);
            const active = route.name === 'wave' && route.id === w.id;
            const displayTitle = waveDisplayTitle(w.title);
            return (
              <WaveRow
                key={w.id}
                wave={w}
                active={active}
                cove={cove ?? null}
                title={cove ? `${cove.name} · ${displayTitle}` : displayTitle}
                onGo={() => onGo({ name: 'wave', id: w.id })}
                onPinWave={onPinWave}
                onDeleteWave={openDeleteWaveDialog}
                rowRef={active ? setActiveWaveRowRef : undefined}
              />
            );
          })}
        </section>
      )}

      <nav className="side-nav side-coves" aria-label="Coves">
        <CovesHeader onCreate={onCreateCove} />
        {coves.map((cove) => {
          const cw = waves.filter((w) => w.coveId === cove.id);
          // Pinned waves intentionally appear in both the quick-access
          // Pinned section and their cove's inline list; pinning is not
          // relocation, and the wave still belongs to this cove.
          const inlineWaves = sortByLifecycleRank(cw);
          const running = cw.filter((w) => isRunning(w.lifecycle)).length;
          // Match the top-of-sidebar "Waiting on you" predicate, including
          // pinned attention waves, so cove warn badges surface pinned work.
          const waiting = cw.filter(waveNeedsUserAttention).length;
          const active = route.name === 'cove' && route.coveId === cove.id;
          const expanded = !!expandedCoves[cove.id];
          const listId = coveWavesListId(cove.id);
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
            <Fragment key={cove.id}>
              <div className="cove-row" role="group">
                <button
                  type="button"
                  className={'cove-row-chevron' + (expanded ? ' expanded' : '')}
                  onClick={(e) => {
                    e.stopPropagation();
                    toggleCoveExpanded(cove.id);
                  }}
                  aria-expanded={expanded}
                  aria-controls={showInlineWaves ? listId : undefined}
                  aria-label={`${expanded ? 'Collapse' : 'Expand'} cove ${cove.name}`}
                >
                  <ChevronIcon />
                </button>
                <button
                  className={'cove-nav' + (active ? ' active' : '')}
                  onClick={() => onGo({ name: 'cove', coveId: cove.id })}
                >
                  <span className="swatch-wrap">
                    <span
                      className={'swatch' + (running > 0 ? ' pulse' : '')}
                      style={{ background: cove.color }}
                    />
                  </span>
                  <span className="lbl">{cove.name}</span>
                  {badge && (
                    <span
                      className={'cove-nav-badge ' + badge.kind}
                      aria-hidden="true"
                    >
                      {badge.n}
                    </span>
                  )}
                </button>
                {onDeleteCove && (
                  <button
                    type="button"
                    className="cove-row-delete"
                    onClick={(e) => {
                      e.stopPropagation();
                      setPendingDelete(cove);
                    }}
                    title={`Delete cove "${cove.name}"`}
                    aria-label={`Delete cove "${cove.name}"`}
                  >
                    <CloseIcon />
                  </button>
                )}
              </div>
              {showInlineWaves && (
                <div
                  id={listId}
                  className="side-coves-waves"
                  role="group"
                  aria-label={`Waves in ${cove.name}`}
                >
                  {inlineWaves.map((w) => {
                    const waveActive = route.name === 'wave' && route.id === w.id;
                    const displayTitle = waveDisplayTitle(w.title);
                    return (
                      <WaveRow
                        key={w.id}
                        wave={w}
                        active={waveActive}
                        cove={null}
                        title={displayTitle}
                        onGo={() => onGo({ name: 'wave', id: w.id })}
                        onPinWave={onPinWave}
                        onDeleteWave={openDeleteWaveDialog}
                        rowRef={waveActive ? setActiveWaveRowRef : undefined}
                      />
                    );
                  })}
                </div>
              )}
            </Fragment>
          );
        })}
      </nav>

      <UserMenu onOpenSettings={onOpenSettings} onSignOut={onSignOut} />

      <ConfirmDialog
        open={pendingDelete !== null}
        title="Delete cove?"
        description={
          pendingDelete
            ? `Delete cove "${pendingDelete.name}"? Its waves and cards go too. This cannot be undone.`
            : null
        }
        confirmLabel="Delete cove"
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

// ---------------- CovesHeader ----------------
//
// Renders the "Coves" section label with a tiny `+` icon button anchored
// on the right edge of the same row. Clicking `+` expands an inline name
// input directly below the header (still at the top of the cove list),
// so the trigger stays in view even when the cove list overflows.

const PALETTE = ['#5a9', '#c97', '#79c', '#b86', '#6a8', '#a6c'];

function CovesHeader({
  onCreate,
}: {
  onCreate?: (name: string, color: string) => void | Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState('');
  const inputRef = useRef<HTMLInputElement | null>(null);

  if (!onCreate) {
    return <div className="nav-label">Coves</div>;
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
        <span>Coves</span>
        <button
          type="button"
          className="nav-label-add"
          onClick={openForm}
          title="New cove"
          aria-label="New cove"
        >
          <PlusIcon />
        </button>
      </div>
      {open && (
        <div className="cove-nav-edit">
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
// A single wave entry in the Pinned, Waiting-on-you, or inline cove list.
// Rendered as `<div role="group">` containing sibling `<button>`s to
// avoid nested-button a11y violations: pin, navigation, and delete.
// The pin button is hover-revealed but always visible when the wave is
// already pinned so unpin is discoverable on touch.

function WaveRow({
  wave,
  active,
  cove,
  title,
  onGo,
  onPinWave,
  onDeleteWave,
  rowRef,
}: {
  wave: Wave;
  active: boolean;
  cove: { id: string; name: string } | null;
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
        {cove && <span className="side-wave-cove">{cove.name}</span>}
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
