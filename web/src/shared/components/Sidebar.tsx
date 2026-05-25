import { useRef } from 'react';
import { useState } from '../state';
import { Menu, type MenuItem } from '../../ui/Menu/Menu';
import { useSession } from '../../app/SessionProvider';
import type { Cove, Route, Wave } from '../../types';
import { isRunning, waveNeedsUserAttention } from '../lifecycle';

// A wave that needs user attention AND is not pinned. Used to keep the
// top-section "Waiting on you" row count and each cove's red badge count
// in parity — both exclude pinned waves so no wave shows a warning badge
// while being invisible from the Waiting section.
function isUnpinnedAndWaiting(w: Wave): boolean {
  return waveNeedsUserAttention(w) && w.pinnedAt == null;
}
import { ConfirmDialog } from '../../ui/ConfirmDialog/ConfirmDialog';
import { CloseIcon } from './CloseIcon';
import { PinIcon } from './PinIcon';
import { PlusIcon } from './PlusIcon';

// ---------------- Sidebar ----------------

export function Sidebar({
  coves,
  waves,
  route,
  onGo,
  onCreateCove,
  onDeleteCove,
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
  const cancelDelete = () => setPendingDelete(null);
  const confirmDelete = async () => {
    const c = pendingDelete;
    setPendingDelete(null);
    if (!c || !onDeleteCove) return;
    await onDeleteCove(c.id);
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
  // Pinned waves are excluded to avoid double-rendering across sections.
  const waitingWaves = waves.filter(isUnpinnedAndWaiting);
  // Sub-landmarks inside the outer <aside aria-label="Navigation">:
  //   <nav aria-label="Sidebar navigation">  → Today button
  //   <section aria-label="Pinned">          → pinned wave rows (when any)
  //   <section aria-label="Waiting on you">  → side-wave rows (when any)
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

      {pinnedWaves.length > 0 && (
        <section className="side-section" aria-label="Pinned">
          <div className="nav-label">Pinned</div>
          {pinnedWaves.map((w) => {
            const cove = coves.find((c) => c.id === w.coveId);
            const active = route.name === 'wave' && route.id === w.id;
            return (
              <WaveRow
                key={w.id}
                wave={w}
                active={active}
                showDot={false}
                title={(cove?.name ?? '') + ' · ' + w.title}
                onGo={() => onGo({ name: 'wave', id: w.id })}
                onPinWave={onPinWave}
              />
            );
          })}
        </section>
      )}

      {waitingWaves.length > 0 && (
        <section className="side-section" aria-label="Waiting on you">
          <div className="nav-label warn-text">Waiting on you</div>
          {waitingWaves.map((w) => {
            const cove = coves.find((c) => c.id === w.coveId);
            const active = route.name === 'wave' && route.id === w.id;
            return (
              <WaveRow
                key={w.id}
                wave={w}
                active={active}
                showDot={true}
                title={(cove?.name ?? '') + ' · ' + w.title}
                onGo={() => onGo({ name: 'wave', id: w.id })}
                onPinWave={onPinWave}
              />
            );
          })}
        </section>
      )}

      <nav className="side-nav side-coves" aria-label="Coves">
        <CovesHeader onCreate={onCreateCove} />
        {coves.map((cove) => {
          const cw = waves.filter((w) => w.coveId === cove.id);
          const running = cw.filter((w) => isRunning(w.lifecycle)).length;
          // Match the top-of-sidebar "Waiting on you" predicate so the
          // per-cove waiting count and the top-section row count agree.
          // isUnpinnedAndWaiting mirrors the waitingWaves filter above,
          // keeping the badge and section counts in parity.
          const waiting = cw.filter(isUnpinnedAndWaiting).length;
          const active = route.name === 'cove' && route.coveId === cove.id;
          // Single right-edge badge slot: warn-red waiting count beats
          // muted total count; empty when there are no waves at all.
          const badge =
            waiting > 0
              ? { kind: 'warn' as const, n: waiting }
              : cw.length > 0
                ? { kind: 'muted' as const, n: cw.length }
                : null;
          return (
            <div key={cove.id} className="cove-row" role="group">
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
// A single wave entry in the Pinned or Waiting-on-you section.
// Rendered as `<div role="group">` containing two sibling `<button>`s
// to avoid nested-button a11y violations: one for navigation, one for
// pin/unpin. The pin button is hover-revealed but always visible when
// the wave is already pinned so unpin is discoverable on touch.

function WaveRow({
  wave,
  active,
  showDot,
  title,
  onGo,
  onPinWave,
}: {
  wave: Wave;
  active: boolean;
  showDot: boolean;
  title: string;
  onGo: () => void;
  onPinWave?: (waveId: string, pin: boolean) => void | Promise<void>;
}) {
  const pinned = wave.pinnedAt != null;
  return (
    <div className={'side-wave-row' + (active ? ' active' : '')} role="group">
      <button
        className={'side-wave' + (active ? ' active' : '')}
        onClick={onGo}
        title={title}
      >
        {showDot && <span className="side-wave-dot" />}
        <span className="side-wave-title">{wave.title}</span>
      </button>
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
          <PinIcon />
        </button>
      )}
    </div>
  );
}
