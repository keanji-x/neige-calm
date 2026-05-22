import { useRef } from 'react';
import { useState } from '../state';
import { Icon } from '../../Icon';
import type { Cove, Route, Wave } from '../../types';

// ---------------- Sidebar ----------------

export function Sidebar({
  coves,
  waves,
  route,
  onGo,
  onCreateCove,
  onOpenSettings,
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
}) {
  const waitingWaves = waves.filter((w) => w.status === 'waiting');
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
          const running = cw.filter((w) => w.status === 'running').length;
          const waiting = cw.filter((w) => w.status === 'waiting').length;
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
      <div className="me-row">
        <span className="me">YK</span>
        <span className="who">
          Yuki K.
          <div className="sub">Pro · 5 agents online</div>
        </span>
        {onOpenSettings && (
          <button
            className="me-settings"
            onClick={onOpenSettings}
            title="Settings"
            aria-label="Open settings"
          >
            <Icon n="gear" s={14} />
          </button>
        )}
      </div>
    </aside>
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
