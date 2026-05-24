import { useRef } from 'react';
import { useState } from '../state';
import { Menu, type MenuItem } from '../../ui/Menu/Menu';
import type { Cove, Route, Wave } from '../../types';
import { isRunning, waveNeedsUserAttention } from '../lifecycle';

// ---------------- Sidebar ----------------

export function Sidebar({
  coves,
  waves,
  route,
  onGo,
  onCreateCove,
  onOpenSettings,
  onSignOut,
}: {
  coves: Cove[];
  waves: Wave[];
  route: Route;
  onGo: (r: Route) => void;
  /** Bootstrap affordance: renders a small `+ New Cove` row below the
   *  Coves list. Lives here (not in CovePage) because creating the *first*
   *  cove has no other home. Wave creation, by contrast, lives inside
   *  CovePage where the cove context is already established. */
  onCreateCove?: (name: string, color: string) => void | Promise<void>;
  /** Open the app-global settings page. Optional so tests / sub-trees that
   *  render the sidebar without a router don't have to wire it up. */
  onOpenSettings?: () => void;
  /** Sign the current user out. Optional so tests / sub-trees that render
   *  the sidebar without a router don't have to wire it up. */
  onSignOut?: () => void;
}) {
  // Issue #254 — OR'd predicate: lifecycle ∪ kernel-card-FSM. Catches
  // both "Spec Agent said blocked/reviewing/failed" AND "a worker card
  // hit an AwaitingInput/Errored hook before Spec Agent could drive
  // lifecycle". The latter is the regression hole #248's deletion of
  // the wave-level FSM union left open.
  const waitingWaves = waves.filter((w) => waveNeedsUserAttention(w));
  // Sub-landmarks inside the outer <aside aria-label="Navigation">:
  //   <nav aria-label="Sidebar navigation">  → Today button
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

      {waitingWaves.length > 0 && (
        <section className="side-section" aria-label="Waiting on you">
          <div className="nav-label warn-text">Waiting on you</div>
          {waitingWaves.map((w) => {
            const cove = coves.find((c) => c.id === w.coveId);
            const active = route.name === 'wave' && route.id === w.id;
            return (
              <button
                key={w.id}
                className={'side-wave' + (active ? ' active' : '')}
                onClick={() => onGo({ name: 'wave', id: w.id })}
                title={(cove?.name ?? '') + ' · ' + w.title}
              >
                <span className="side-wave-dot" />
                <span className="side-wave-title">{w.title}</span>
              </button>
            );
          })}
        </section>
      )}

      <nav className="side-nav" aria-label="Coves">
        <div className="nav-label">Coves</div>
        {coves.map((cove) => {
          const cw = waves.filter((w) => w.coveId === cove.id);
          const running = cw.filter((w) => isRunning(w.lifecycle)).length;
          // Match the top-of-sidebar "Waiting on you" predicate so the
          // per-cove pip count and the top-section row count agree.
          const waiting = cw.filter((w) => waveNeedsUserAttention(w)).length;
          const active = route.name === 'cove' && route.coveId === cove.id;
          return (
            <button
              key={cove.id}
              className={'cove-nav' + (active ? ' active' : '')}
              onClick={() => onGo({ name: 'cove', coveId: cove.id })}
            >
              <span className="swatch-wrap">
                <span
                  className={'swatch' + (running > 0 ? ' pulse' : '')}
                  style={{ background: cove.color }}
                />
                {waiting > 0 && (
                  <span className="pip" aria-hidden="true">
                    {waiting}
                  </span>
                )}
              </span>
              <span className="lbl">{cove.name}</span>
              {cw.length > 0 && (
                <span className="count" aria-hidden="true">
                  {cw.length}
                </span>
              )}
            </button>
          );
        })}
        {onCreateCove && <NewCoveButton onCreate={onCreateCove} />}
      </nav>

      <span className="sp" />
      <UserMenu onOpenSettings={onOpenSettings} onSignOut={onSignOut} />
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
//
// The trigger must be a single focusable <button>, so the `.sub` line
// inside `.who` is a <span> with `display: block` (set in calm.css) —
// no block elements inside a button per HTML.
function UserMenu({
  onOpenSettings,
  onSignOut,
}: {
  onOpenSettings?: () => void;
  onSignOut?: () => void;
}) {
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
          <span className="me">YK</span>
          <span className="who">
            Yuki K.
            <span className="sub">Pro · 5 agents online</span>
          </span>
        </button>
      )}
    />
  );
}

// ---------------- NewCoveButton ----------------
//
// Lives in the sidebar because creating the *first* cove has no other home;
// every subsequent affordance (new wave, new card) lives inside the page
// it belongs to. Bootstraps a random color from a fixed palette — a real
// color picker can land in a settings/command-palette pass later.

const PALETTE = ['#5a9', '#c97', '#79c', '#b86', '#6a8', '#a6c'];

function NewCoveButton({
  onCreate,
}: {
  onCreate: (name: string, color: string) => void | Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [name, setName] = useState('');
  const inputRef = useRef<HTMLInputElement | null>(null);

  // When the inline form opens, focus the input on the next tick so the
  // ref is bound. Cheaper than a separate effect for one-shot focus.
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

  if (!open) {
    return (
      <button className="cove-nav new" onClick={openForm} title="New cove">
        <span className="swatch-wrap">
          <span className="swatch-plus">+</span>
        </span>
        <span className="lbl">New cove</span>
      </button>
    );
  }
  return (
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
  );
}
