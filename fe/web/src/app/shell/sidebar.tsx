// The workspace rail. Three sections in a fixed order; see INV-SIDEBAR-007.

import { useEffect, useRef } from 'react';

import { coveOf, visibleCoves, type Cove } from '../../../../core/domain/cove.ts';
import { isRunning, isWaitingForUser, type Wave } from '../../../../core/domain/wave.ts';
import { COVE_PALETTE } from '../../features/cove/palette.ts';
import { WaveRow } from '../../features/wave/row/public.tsx';
import { DELETE_COVE_COPY, DELETE_WAVE_COPY } from '../../ui/confirm-dialog/copy.ts';
import { ConfirmDialog } from '../../ui/dialog/public.tsx';
import { Menu } from '../../ui/menu/public.tsx';
import { useState } from '../../ui/state/public.ts';
import type { NavTarget } from '../router/navigation.ts';
import { useTheme } from '../theme/public.tsx';
import styles from './shell.module.css';

const NEXT_THEME_MODE = Object.freeze({ system: 'light', light: 'dark', dark: 'system' } as const);

export type SidebarProps = Readonly<{
  coves: readonly Cove[];
  wavesByCove: ReadonlyMap<string, readonly Wave[]>;
  waves: readonly Wave[];
  currentPath: string;
  onGo: (target: NavTarget) => void;
  /** Colour is picked here, at random from `COVE_PALETTE` (INV-DUP-006). */
  onCreateCove: (name: string, color: string) => void | Promise<void>;
  onDeleteCove: (coveId: string) => void | Promise<void>;
  onSetPinned: (waveId: string, pinned: boolean) => void | Promise<void>;
  onDeleteWave: (waveId: string) => void | Promise<void>;
  onOpenSettings: () => void;
  /** The shell never signs out itself; the owner of the session does. */
  onSignOut: () => void;
  userLabel?: string;
}>;

/** Two initials at most; the avatar is decoration on top of a labelled button. */
export function initialsOf(label: string): string {
  const parts = label.split(/\s+/).filter((part) => part.length > 0);
  return parts.slice(0, 2).map((part) => part[0]?.toUpperCase() ?? '').join('') || '?';
}

function randomCoveColor(): string {
  return COVE_PALETTE[Math.floor(Math.random() * COVE_PALETTE.length)] ?? COVE_PALETTE[0];
}

/**
 * INV-CONFIRM-001 — the destructive confirm stays mounted for the whole await:
 * Confirm goes disabled, Cancel stays enabled (the user must keep an exit), and
 * the `finally` clears both pending and target so a *rejected* mutation cannot
 * strand the dialog in a permanently-disabled state.
 */
function useDeleteConfirm(perform: (id: string) => void | Promise<void>) {
  const [target, setTarget] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  return {
    open: target !== null,
    pending,
    request: (id: string) => setTarget(id),
    cancel: () => { if (!pending) setTarget(null); },
    confirm: () => {
      if (pending || target === null) return;
      setPending(true);
      void (async () => {
        try {
          await perform(target);
        } catch {
          // The caller owns surfacing the failure; this surface only has to
          // make sure the dialog cannot strand. See INV-CONFIRM-001.
        } finally {
          setPending(false);
          setTarget(null);
        }
      })();
    },
  };
}

/**
 * INV-SIDEBAR-007 — the three sections render in this order, and **pinning is
 * not relocation**: a pinned wave appears in the Pinned section *and* in its
 * cove's inline list, and if it also needs attention it appears in "Waiting on
 * you" as well. Waiting deliberately includes pinned attention waves.
 *
 * INV-A11Y-058 — there is **intentionally no skip-to-main link**. The rail is
 * short enough that it has not been raised as a pain point (a11y contract
 * §3.1/§9). If a second rail section with many rows lands, re-evaluate; until
 * then "there is no skip link" is a decision, not a defect.
 *
 * INV-SIDEBAR-012 — every row's pin button is hover-revealed while the wave is
 * unpinned and permanently visible once it is pinned; the reveal itself is CSS
 * (`features/wave/row/row.module.css`), so a jsdom test can only prove the
 * control is always in the accessibility tree and carries `aria-pressed`.
 *
 * E2E-INV-SHELL-003 — `visibleCoves` filters the kernel system cove here as
 * well as in the query layer, so scaffolding cannot reach the rail even if a
 * caller hands over an unfiltered list.
 */
export function Sidebar({
  coves, wavesByCove, waves, currentPath, onGo,
  onCreateCove, onDeleteCove, onSetPinned, onDeleteWave,
  onOpenSettings, onSignOut, userLabel = 'You',
}: SidebarProps) {
  const { mode, resolved, setMode } = useTheme();
  const [collapsed, setCollapsed] = useState(false);
  const [expandedOverride, setExpandedOverride] = useState<ReadonlyMap<string, boolean>>(() => new Map());
  const [creatingCove, setCreatingCove] = useState(false);
  const [coveDraft, setCoveDraft] = useState('');
  const canceledRef = useRef(false);
  const coveInputRef = useRef<HTMLInputElement | null>(null);
  const waveConfirm = useDeleteConfirm(onDeleteWave);
  const coveConfirm = useDeleteConfirm(onDeleteCove);

  const userCoves = visibleCoves(coves);
  const userCoveIds = new Set(userCoves.map((cove) => cove.id));
  const visibleWaves = waves.filter((wave) => userCoveIds.has(wave.coveId));
  const waiting = visibleWaves.filter((wave) => isWaitingForUser(wave.lifecycle));
  const pinned = visibleWaves.filter((wave) => wave.pinnedAt !== null)
    .toSorted((left, right) => (right.pinnedAt ?? 0) - (left.pinnedAt ?? 0));

  const activeWaveId = currentPath.startsWith('/wave/') ? currentPath.slice('/wave/'.length) : null;
  const activeCoveId = activeWaveId === null
    ? null
    : visibleWaves.find((wave) => wave.id === activeWaveId)?.coveId ?? null;

  // Navigating into a wave drops any manual collapse on its cove — the row the
  // user just opened has to be visible. Dropping the override (rather than
  // forcing `true`) keeps the chevron usable straight afterwards.
  useEffect(() => {
    if (activeCoveId === null) return;
    setExpandedOverride((current) => {
      if (!current.has(activeCoveId)) return current;
      const next = new Map(current);
      next.delete(activeCoveId);
      return next;
    });
  }, [activeCoveId]);

  useEffect(() => { if (creatingCove) coveInputRef.current?.focus(); }, [creatingCove]);

  const submitCove = () => {
    const name = coveDraft.trim();
    setCreatingCove(false);
    setCoveDraft('');
    if (name === '') return;
    void onCreateCove(name, randomCoveColor());
  };
  const cancelCove = () => { canceledRef.current = true; setCreatingCove(false); setCoveDraft(''); };

  const rowProps = {
    currentPath,
    onGo,
    onSetPinned: (waveId: string, next: boolean) => { void onSetPinned(waveId, next); },
    onDelete: waveConfirm.request,
  };

  return (
    <nav className={`${styles.rail} ${collapsed ? styles.railCollapsed : ''}`} aria-label="Workspace">
      <div className={styles.brandRow}>
        {!collapsed && (
          <button type="button" className={styles.brand} onClick={() => onGo({ name: 'today' })}>
            neige · calm
          </button>
        )}
        <button
          type="button"
          className={styles.collapseToggle}
          aria-label={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
          aria-expanded={!collapsed}
          onClick={() => setCollapsed(!collapsed)}
        >
          {collapsed ? '»' : '«'}
        </button>
        {!collapsed && (
          <button
            type="button"
            className={styles.themeToggle}
            aria-label={`Theme: ${mode} (resolved ${resolved})`}
            onClick={() => setMode(NEXT_THEME_MODE[mode])}
          >
            {mode}
          </button>
        )}
      </div>

      {collapsed ? (
        <div className={styles.iconStrip}>
          {userCoves.map((cove) => (
            <button
              key={cove.id}
              type="button"
              className={styles.iconCove}
              aria-label={cove.name}
              aria-current={currentPath === `/cove/${cove.id}` ? 'page' : undefined}
              onClick={() => onGo({ name: 'cove', coveId: cove.id })}
            >
              <span className={styles.swatch} style={{ background: cove.color }} aria-hidden="true" />
            </button>
          ))}
        </div>
      ) : (
        <>
          <WaveSection title="Waiting on you" waves={waiting} coves={userCoves} {...rowProps} />
          <WaveSection title="Pinned" waves={pinned} coves={userCoves} {...rowProps} />

          <div className={styles.section}>
            <div className={styles.sectionHead}>
              <h2 className={styles.sectionTitle}>Coves</h2>
              <button
                type="button"
                className={styles.newCove}
                aria-label="New cove"
                onClick={() => { canceledRef.current = false; setCoveDraft(''); setCreatingCove(true); }}
              >
                +
              </button>
            </div>

            {creatingCove && (
              <input
                ref={coveInputRef}
                type="text"
                className={styles.coveInput}
                aria-label="Cove name"
                value={coveDraft}
                onChange={(event) => setCoveDraft(event.target.value)}
                onKeyDown={(event) => {
                  if (event.key === 'Enter') { event.preventDefault(); submitCove(); }
                  else if (event.key === 'Escape') { event.preventDefault(); cancelCove(); }
                }}
                onBlur={() => {
                  if (canceledRef.current) { canceledRef.current = false; return; }
                  submitCove();
                }}
              />
            )}

            {userCoves.length === 0 && <p className={styles.empty}>No coves yet.</p>}
            {userCoves.map((cove) => (
              <CoveGroup
                key={cove.id}
                cove={cove}
                coveWaves={wavesByCove.get(cove.id) ?? []}
                expanded={expandedOverride.get(cove.id) ?? true}
                onToggle={(next) => setExpandedOverride((current) => new Map(current).set(cove.id, next))}
                onRequestDelete={coveConfirm.request}
                {...rowProps}
              />
            ))}
          </div>

          <div className={styles.userRow}>
            <Menu
              items={[
                { label: 'Settings', onSelect: onOpenSettings },
                { label: 'Sign out', onSelect: onSignOut },
              ]}
              wrapClassName={styles.menuWrap}
              menuClassName={styles.menu}
              itemClassName={styles.menuItem}
              trigger={(triggerProps) => (
                <button
                  {...triggerProps}
                  type="button"
                  className={styles.avatar}
                  aria-label={`Account menu for ${userLabel}`}
                >
                  {initialsOf(userLabel)}
                </button>
              )}
            />
          </div>
        </>
      )}

      <ConfirmDialog
        open={waveConfirm.open}
        title={DELETE_WAVE_COPY.title}
        description={DELETE_WAVE_COPY.description}
        confirmLabel={DELETE_WAVE_COPY.confirmLabel}
        confirmDisabled={waveConfirm.pending}
        onConfirm={waveConfirm.confirm}
        onCancel={waveConfirm.cancel}
      />
      <ConfirmDialog
        open={coveConfirm.open}
        title={DELETE_COVE_COPY.title}
        description={DELETE_COVE_COPY.description}
        confirmLabel={DELETE_COVE_COPY.confirmLabel}
        confirmDisabled={coveConfirm.pending}
        onConfirm={coveConfirm.confirm}
        onCancel={coveConfirm.cancel}
      />
    </nav>
  );
}

type RowProps = Readonly<{
  currentPath: string;
  onGo: (target: NavTarget) => void;
  onSetPinned: (waveId: string, pinned: boolean) => void;
  onDelete: (waveId: string) => void;
}>;

function WaveSection({ title, waves, coves, currentPath, onGo, onSetPinned, onDelete }: RowProps & {
  title: string;
  waves: readonly Wave[];
  coves: readonly Cove[];
}) {
  if (waves.length === 0) return null;
  return (
    <div className={styles.section}>
      <h2 className={styles.sectionTitle}>{title}</h2>
      {waves.map((wave) => (
        <WaveRow
          key={wave.id}
          wave={wave}
          coveName={coveOf(wave.coveId, coves)?.name}
          coveColor={coveOf(wave.coveId, coves)?.color}
          compact
          active={currentPath === `/wave/${wave.id}`}
          onOpen={(waveId) => onGo({ name: 'wave', waveId })}
          onSetPinned={onSetPinned}
          onDelete={onDelete}
        />
      ))}
    </div>
  );
}

/** INV-A11Y-061 — navigation is `<button>` + `onGo`, never a native `<a href>`. */
function CoveGroup({
  cove, coveWaves, expanded, onToggle, onRequestDelete, currentPath, onGo, onSetPinned, onDelete,
}: RowProps & {
  cove: Cove;
  coveWaves: readonly Wave[];
  expanded: boolean;
  onToggle: (expanded: boolean) => void;
  onRequestDelete: (coveId: string) => void;
}) {
  const active = currentPath === `/cove/${cove.id}`;
  const waitingCount = coveWaves.filter((wave) => isWaitingForUser(wave.lifecycle)).length;
  const running = coveWaves.some((wave) => isRunning(wave.lifecycle));

  return (
    <div className={styles.coveGroup}>
      <div className={styles.coveRowWrap}>
        <button
          type="button"
          className={styles.chevron}
          aria-expanded={expanded}
          aria-label={`${expanded ? 'Collapse' : 'Expand'} cove ${cove.name}`}
          onClick={() => onToggle(!expanded)}
        >
          {expanded ? '▾' : '▸'}
        </button>
        <button
          type="button"
          className={`${styles.coveRow} ${active ? styles.rowActive : ''}`}
          aria-current={active ? 'page' : undefined}
          onClick={() => onGo({ name: 'cove', coveId: cove.id })}
        >
          <span
            className={`${styles.swatch} ${running ? styles.swatchRunning : ''}`}
            style={{ background: cove.color }}
            aria-hidden="true"
          />
          <span className={styles.rowLabel}>{cove.name}</span>
          {/* The row's accessible name already carries the cove name; the count
              is redundant decoration for a screen reader. */}
          {coveWaves.length > 0 && (
            <span className={waitingCount > 0 ? styles.countWarn : styles.count} aria-hidden="true">
              {waitingCount > 0 ? waitingCount : coveWaves.length}
            </span>
          )}
        </button>
        <button
          type="button"
          className={styles.coveDelete}
          aria-label={`Delete cove ${cove.name}`}
          onClick={() => onRequestDelete(cove.id)}
        >
          ×
        </button>
      </div>
      {expanded && (
        <div className={styles.waveList}>
          {coveWaves.map((wave) => (
            <WaveRow
              key={wave.id}
              wave={wave}
              compact
              active={currentPath === `/wave/${wave.id}`}
              onOpen={(waveId) => onGo({ name: 'wave', waveId })}
              onSetPinned={onSetPinned}
              onDelete={onDelete}
            />
          ))}
        </div>
      )}
    </div>
  );
}
