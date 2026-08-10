// The workspace rail. Three sections in a fixed order; see INV-SIDEBAR-007.
//
// §7.5 — the rail has exactly one emphasis rule: only the current location may
// use `--accent`, and only "WAITING ON YOU" may show `--warn`. Everything else
// is greyscale. A sidebar with three colours of status is a status board, not
// navigation.

import { useEffect, useRef } from 'react';

import { coveOf, visibleCoves, type Cove } from '../../../../core/domain/cove.ts';
import { needsUserAttention, type Wave } from '../../../../core/domain/wave.ts';
import { COVE_PALETTE } from '../../features/cove/palette.ts';
import { WaveRow } from '../../features/wave/row/public.tsx';
import { deleteCoveCopy, DELETE_WAVE_COPY } from '../../ui/confirm-dialog/copy.ts';
import { ConfirmDialog } from '../../ui/dialog/public.tsx';
import { Menu } from '../../ui/menu/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { TypedDeleteBody, useTypedConfirm } from '../../ui/typed-confirm/public.tsx';
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
  /** Owned by the shell: collapsing changes the shell grid, not just the rail. */
  collapsed: boolean;
  onToggleCollapsed: () => void;
  /** CR-8 — where focus lands once a delete removes the trigger element. */
  pageTitleRef?: React.RefObject<HTMLElement | null>;
  userLabel?: string;
  nowMs?: number;
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
 * Confirm goes busy, Cancel stays enabled (the user must keep an exit), and the
 * `finally` clears both pending and target so a *rejected* mutation cannot
 * strand the dialog.
 *
 * Since CR-6 the in-flight state is `busy`, not a real `disabled`: a disabled
 * element is not focusable, so pressing Confirm would drop focus out of the
 * trap at the exact moment the dialog needs to hold it.
 */
function useDeleteConfirm(perform: (id: string) => void | Promise<void>, onDone?: () => void) {
  const [target, setTarget] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  return {
    target,
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
          // Order is load-bearing (CR-8): navigate first, then close, so the new
          // page's title element is mounted when the dialog's cleanup restores
          // focus to it.
          onDone?.();
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
 * §3.1/§9). If a second rail section with many rows lands, re-evaluate.
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
  onOpenSettings, onSignOut, collapsed, onToggleCollapsed, pageTitleRef,
  userLabel = 'You', nowMs,
}: SidebarProps) {
  const { mode, resolved, setMode } = useTheme();
  const [expandedOverride, setExpandedOverride] = useState<ReadonlyMap<string, boolean>>(() => new Map());
  const [creatingCove, setCreatingCove] = useState(false);
  const [coveDraft, setCoveDraft] = useState('');
  const coveInputRef = useRef<HTMLInputElement | null>(null);
  const waveConfirm = useDeleteConfirm(onDeleteWave);
  const coveConfirm = useDeleteConfirm(onDeleteCove, () => onGo({ name: 'today' }));

  const userCoves = visibleCoves(coves);
  const userCoveIds = new Set(userCoves.map((cove) => cove.id));
  const visibleWaves = waves.filter((wave) => userCoveIds.has(wave.coveId));
  const waiting = visibleWaves.filter(needsUserAttention);
  const pinned = visibleWaves.filter((wave) => wave.pinnedAt !== null)
    .toSorted((left, right) => (right.pinnedAt ?? 0) - (left.pinnedAt ?? 0));

  const activeWaveId = currentPath.startsWith('/wave/') ? currentPath.slice('/wave/'.length) : null;
  const activeCoveId = activeWaveId === null
    ? null
    : visibleWaves.find((wave) => wave.id === activeWaveId)?.coveId ?? null;

  const deletingCove = userCoves.find((cove) => cove.id === coveConfirm.target);
  const typed = useTypedConfirm(deletingCove?.name ?? '');
  const coveCopy = deleteCoveCopy(
    deletingCove?.name ?? '',
    wavesByCove.get(coveConfirm.target ?? '')?.length ?? 0,
  );

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
    if (name === '') return;
    setCreatingCove(false);
    setCoveDraft('');
    void onCreateCove(name, randomCoveColor());
  };

  const rowProps = {
    currentPath,
    onGo,
    nowMs,
    onSetPinned: (waveId: string, next: boolean) => { void onSetPinned(waveId, next); },
    onDelete: waveConfirm.request,
  };

  // The rail has no cove yet: the one remedy is to make one, so the input opens
  // in the first row's place rather than a sentence pointing at a button
  // elsewhere (§5.3). It is not auto-focused — that would steal the reading
  // position a screen-reader user just landed on.
  const showInlineCreate = creatingCove || userCoves.length === 0;

  return (
    <nav className={`${styles.rail} ${collapsed ? styles.railCollapsed : ''}`} aria-label="Workspace">
      <div className={styles.brandRow}>
        {!collapsed && (
          <button type="button" data-nc-role="row" className={styles.brand} onClick={() => onGo({ name: 'today' })}>
            neige · calm
          </button>
        )}
        <button
          type="button"
          data-nc-role="icon"
          className={`${styles.iconButton} ${collapsed ? '' : styles.spring}`}
          aria-label={collapsed ? 'Expand sidebar' : 'Collapse sidebar'}
          aria-expanded={!collapsed}
          onClick={onToggleCollapsed}
        >
          {collapsed ? '›' : '‹'}
        </button>
      </div>

      {collapsed ? (
        <>
          {/* The one number in the collapsed rail, and the only colour in it.
              §7.5 allows exactly two exceptions to greyscale — the current
              location and "waiting on you" — and the tone alone carries this
              one, so the warn dot that used to sit beside it is gone: a
              coloured digit and a coloured dot said the same thing twice. */}
          {waiting.length > 0 && (
            <div className={styles.stripWaiting} aria-label={`${waiting.length} waiting on you`}>
              {waiting.length}
            </div>
          )}
          {/* An initial, not a colour chip. Eight cove hues stacked down a 44px
              strip turned navigation into a palette — and they were the app's
              only use of `--cove-*` outside the surfaces that genuinely mix
              coves (Today's agenda, the calendar day dot). A letter says which
              cove without spending a channel §7.5 reserves for state, and the
              current one is still marked the way every other row marks it:
              `--accent-soft` fill. */}
          {userCoves.map((cove) => (
            <button
              key={cove.id}
              type="button"
              data-nc-role="row"
              className={`${styles.stripItem} ${currentPath === `/cove/${cove.id}` ? styles.stripItemActive : ''}`}
              aria-label={cove.name}
              title={cove.name}
              aria-current={currentPath === `/cove/${cove.id}` ? 'page' : undefined}
              onClick={() => onGo({ name: 'cove', coveId: cove.id })}
            >
              <span className={styles.stripInitial} aria-hidden="true">{initialsOf(cove.name)[0]}</span>
            </button>
          ))}
        </>
      ) : (
        <>
          <WaveSection title="Waiting on you" waves={waiting} coves={userCoves} {...rowProps} />
          <WaveSection title="Pinned" waves={pinned} coves={userCoves} {...rowProps} />

          <div className={styles.section}>
            <div className={styles.sectionHead}>
              <h2 className={styles.sectionTitle}>Coves</h2>
              <button
                type="button"
                data-nc-role="icon"
                className={styles.sectionAction}
                aria-label="New cove"
                onClick={() => { setCoveDraft(''); setCreatingCove(true); }}
              >
                +
              </button>
            </div>

            {showInlineCreate && (
              <input
                ref={coveInputRef}
                type="text"
                className={styles.inlineCreate}
                aria-label="Cove name"
                placeholder="New cove…"
                value={coveDraft}
                onChange={(event) => setCoveDraft(event.target.value)}
                /* §6.12 — an inline editor commits on blur, like the title
                   editor does. Clicking away from a field you have typed into
                   and losing the text is the behaviour people learn to distrust
                   inline editing for. Escape is the discard, and it clears the
                   draft first so this handler has nothing left to submit. */
                onBlur={submitCove}
                onKeyDown={(event) => {
                  if (event.key === 'Enter') { event.preventDefault(); submitCove(); }
                  else if (event.key === 'Escape') {
                    event.preventDefault();
                    setCreatingCove(false);
                    setCoveDraft('');
                  }
                }}
              />
            )}

            {userCoves.length > 0 && (
              <div className={styles.coveGroups}>
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
            )}
          </div>

          <div className={styles.userRow}>
            <Menu
              items={[
                { label: `Theme: ${mode} (${resolved})`, onSelect: () => setMode(NEXT_THEME_MODE[mode]) },
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
                  data-nc-role="icon"
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
        confirmBusyLabel="Deleting…"
        confirmState={waveConfirm.pending ? 'busy' : 'ready'}
        restoreFocusRef={pageTitleRef}
        onConfirm={waveConfirm.confirm}
        onCancel={waveConfirm.cancel}
      />
      {/* Deleting a cove cascades to every wave inside it: the one operation in
          the product that earns a typed confirm (§4.3). The rail entry and the
          cove page header entry are two entry points to the same operation, so
          they share this dialog's copy and its confirmation strength. */}
      <ConfirmDialog
        open={coveConfirm.open}
        title={coveCopy.title}
        description={<TypedDeleteBody
          copy={coveCopy}
          expected={deletingCove?.name ?? ''}
          value={typed.value}
          inputRef={typed.inputRef}
          onChange={typed.setValue}
        />}
        confirmLabel={coveCopy.confirmLabel}
        confirmBusyLabel="Deleting…"
        confirmState={coveConfirm.pending ? 'busy' : (typed.matches ? 'ready' : 'blocked')}
        initialFocusRef={typed.inputRef}
        restoreFocusRef={pageTitleRef}
        onConfirm={coveConfirm.confirm}
        onCancel={coveConfirm.cancel}
      />
    </nav>
  );
}

type RowProps = Readonly<{
  currentPath: string;
  onGo: (target: NavTarget) => void;
  nowMs?: number;
  onSetPinned: (waveId: string, pinned: boolean) => void;
  onDelete: (waveId: string) => void;
}>;

/** A section with no rows does not render at all — no label, no dashed box.
 *  That absence is why the rail looks empty when nothing needs you (§6.1). */
function WaveSection({ title, waves, coves, currentPath, onGo, nowMs, onSetPinned, onDelete }: RowProps & {
  title: string;
  waves: readonly Wave[];
  coves: readonly Cove[];
}) {
  if (waves.length === 0) return null;
  return (
    <div className={styles.section}>
      <h2 className={styles.sectionTitle}>{title}</h2>
      <div className={styles.sectionRows}>
        {waves.map((wave) => (
          <WaveRow
            key={wave.id}
            wave={wave}
            coveName={coveOf(wave.coveId, coves)?.name}
            variant="rail"
            nowMs={nowMs}
            active={currentPath === `/wave/${wave.id}`}
            onOpen={(waveId) => onGo({ name: 'wave', waveId })}
            onSetPinned={onSetPinned}
            onDelete={onDelete}
          />
        ))}
      </div>
    </div>
  );
}

/** INV-A11Y-061 — navigation is `<button>` + `onGo`, never a native `<a href>`. */
function CoveGroup({
  cove, coveWaves, expanded, onToggle, onRequestDelete, currentPath, onGo, nowMs, onSetPinned, onDelete,
}: RowProps & {
  cove: Cove;
  coveWaves: readonly Wave[];
  expanded: boolean;
  onToggle: (expanded: boolean) => void;
  onRequestDelete: (coveId: string) => void;
}) {
  const active = currentPath === `/cove/${cove.id}`;

  return (
    <div className={styles.coveGroup}>
      <div className={styles.coveRowWrap}>
        <button
          type="button"
          data-nc-role="row"
          className={`${styles.coveRow} ${active ? styles.coveRowActive : ''}`}
          aria-current={active ? 'page' : undefined}
          onClick={() => onGo({ name: 'cove', coveId: cove.id })}
        >
          {/* Name only. The chevron is a sibling positioned over this row's
              leading gutter (a button inside a button is invalid HTML and trips
              axe's `nested-interactive`); there is no identity dot, no wave
              count, and — since the wave row's status dot moved to the trailing
              edge — no empty status cell either. */}
          <span className={styles.coveName} title={cove.name}>{cove.name}</span>
        </button>
        <button
          type="button"
          data-nc-role="icon"
          className={`${styles.chevron} ${expanded ? styles.chevronOpen : ''}`}
          aria-expanded={expanded}
          aria-label={`${expanded ? 'Collapse' : 'Expand'} cove ${cove.name}`}
          onClick={() => onToggle(!expanded)}
        >
          {/* One stroked chevron that rotates, not a filled ▸/▾ pair. §2.6
              already names "折叠箭头旋转" as a --motion-snappy case, and a solid
              triangle is the heaviest mark in a rail meant to read as line work. */}
          ›
        </button>
        <button
          type="button"
          data-nc-role="icon"
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
              variant="rail"
              nowMs={nowMs}
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
