// The workspace rail. Three sections in a fixed order; see INV-SIDEBAR-007.
//
// §7.5 — the rail has exactly one emphasis rule: only the current location may
// use `--accent`, and only "WAITING ON YOU" may show `--warn`. Everything else
// is greyscale. A sidebar with three colours of status is a status board, not
// navigation.

import { useEffect, useRef } from 'react';

import { coveOf, visibleCoves, type Cove } from '../../../../core/domain/cove.ts';
import { needsUserAttention, userVisibleWaves, visibleWaves, type Wave } from '../../../../core/domain/wave.ts';
import { COVE_PALETTE } from '../../features/cove/palette.ts';
import { WaveRow } from '../../features/wave/row/public.tsx';
import { deleteCoveCopy, DELETE_WAVE_COPY } from '../../ui/confirm-dialog/copy.ts';
import { ConfirmDialog } from '../../ui/dialog/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { Menu } from '../../ui/menu/public.tsx';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import {
  OperationFeedback, useDeleteConfirm, useOperationFeedback,
} from '../../ui/operation-feedback/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { TypedDeleteBody, useTypedConfirm } from '../../ui/typed-confirm/public.tsx';
import type { NavTarget } from '../router/navigation.ts';
import { routeParamFromPath } from '../router/navigation.ts';
import styles from './shell.module.css';

export type SidebarProps = Readonly<{
  coves: readonly Cove[];
  wavesByCove: ReadonlyMap<string, readonly Wave[]>;
  waves: readonly Wave[];
  currentPath: string;
  onGo: (target: NavTarget) => void;
  /** Colour is picked here, at random from `COVE_PALETTE` (INV-DUP-006). */
  onCreateCove: (name: string, color: string) => void | Promise<void>;
  onDeleteCove: (coveId: string, signal: AbortSignal) => void | Promise<void>;
  /** Opens the shell's New wave dialog for this cove (hidden on the POST, not
   *  a picker). The rail does not own the dialog — `AppShell` does, because
   *  the cove page's `+` opens the same one. */
  onNewWave: (coveId: string) => void;
  onSetPinned: (waveId: string, pinned: boolean) => void | Promise<void>;
  onDeleteWave: (waveId: string, signal: AbortSignal) => void | Promise<void>;
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

function randomCoveColor(): string {
  return COVE_PALETTE[Math.floor(Math.random() * COVE_PALETTE.length)] ?? COVE_PALETTE[0];
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
 * E2E-INV-SHELL-003 — `userVisibleWaves` filters the kernel system cove here
 * as well as in the query layer, so scaffolding cannot reach the rail even if a
 * caller hands over an unfiltered list. It is the same function mobile Pages
 * uses, so the two surfaces cannot drift (#1191 §3.1).
 */
export function Sidebar({
  coves, wavesByCove, waves, currentPath, onGo,
  onCreateCove, onDeleteCove, onNewWave, onSetPinned, onDeleteWave,
  onOpenSettings, onOpenPlugins, onSignOut, collapsed, onToggleCollapsed,
  userLabel = 'You', nowMs, readError = null, activityError = null,
  readLoading = false, onRetryRead = () => undefined,
}: SidebarProps) {
  const [expandedOverride, setExpandedOverride] = useState<ReadonlyMap<string, boolean>>(() => new Map());
  const [creatingCove, setCreatingCove] = useState(false);
  const [coveDraft, setCoveDraft] = useState('');
  const coveInputRef = useRef<HTMLInputElement | null>(null);
  const railRef = useRef<HTMLElement | null>(null);
  const waveConfirm = useDeleteConfirm(onDeleteWave);
  const coveConfirm = useDeleteConfirm(onDeleteCove, () => onGo({ name: 'today' }));
  const writeFeedback = useOperationFeedback();

  const userCoves = visibleCoves(coves);
  const userWaves = userVisibleWaves(waves, coves);
  const waiting = userWaves.filter(needsUserAttention);
  const pinned = userWaves.filter((wave) => wave.pinnedAt !== null)
    .toSorted((left, right) => (right.pinnedAt ?? 0) - (left.pinnedAt ?? 0));

  const activeWaveId = routeParamFromPath(currentPath, '/wave/') ?? null;
  const activeCoveId = activeWaveId === null
    ? null
    : userWaves.find((wave) => wave.id === activeWaveId)?.coveId ?? null;

  const deletingCove = userCoves.find((cove) => cove.id === coveConfirm.target);
  const typed = useTypedConfirm(deletingCove?.name ?? '');
  const coveCopy = deleteCoveCopy(
    deletingCove?.name ?? '',
    wavesByCove.get(coveConfirm.target ?? '')?.length,
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

  /*
   * …and then brings it into view. Expanding the cove is only half of "show me
   * where I am": a workspace with a dozen coves puts the open wave below the
   * fold as often as not, and the rail then shows an expanded cove with nothing
   * marked in it.
   *
   * The target is found by `aria-current="page"`, which is the same fact the
   * highlight is drawn from rather than a second copy of it — there is exactly
   * one such row now that the shortcut sections no longer claim to be current.
   *
   * `block: 'nearest'` scrolls only when the row is actually outside the
   * viewport, so arriving at a wave already on screen moves nothing (principle
   * 3). It re-runs on `expandedOverride` too, because the effect above may have
   * only just expanded the cove the row lives in.
   */
  useEffect(() => {
    if (activeWaveId === null) return;
    railRef.current?.querySelector('[aria-current="page"]')
      ?.scrollIntoView?.({ block: 'nearest' });
  }, [activeWaveId, expandedOverride]);

  useEffect(() => { if (creatingCove) coveInputRef.current?.focus(); }, [creatingCove]);

  const submitCove = () => {
    const name = coveDraft.trim();
    if (name === '') return;
    setCreatingCove(false);
    setCoveDraft('');
    void writeFeedback.run(Promise.resolve(onCreateCove(name, randomCoveColor())), 'Could not create the cove.');
  };

  const rowProps = {
    currentPath,
    onGo,
    nowMs,
    onSetPinned: (waveId: string, next: boolean) => {
      void writeFeedback.run(Promise.resolve(onSetPinned(waveId, next)), 'Could not update the wave.');
    },
    onDelete: waveConfirm.request,
  };

  // The rail has no cove yet: the one remedy is to make one, so the input opens
  // in the first row's place rather than a sentence pointing at a button
  // elsewhere (§5.3). It is not auto-focused — that would steal the reading
  // position a screen-reader user just landed on.
  const showInlineCreate = readError === null && !readLoading && (creatingCove || userCoves.length === 0);

  return (
    <nav ref={railRef} className={`${styles.rail} ${collapsed ? styles.railCollapsed : ''}`} aria-label="Workspace">
      <div className={styles.brandRow}>
        {!collapsed && (
          <button type="button" data-nc-role="row" className={styles.brand} onClick={() => onGo({ name: 'today' })}>
            <span className={styles.brandMark} aria-hidden="true" />
            <span className={styles.brandText}>neige · calm</span>
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
      {activityError !== null && <ErrorBox message={`Wave activity is unavailable: ${activityError}`} onRetry={onRetryRead} />}
      {readLoading && <div role="status">Loading workspace…</div>}
      <OperationFeedback feedback={writeFeedback} />

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
              className={`${styles.stripItem} ${routeParamFromPath(currentPath, '/cove/') === cove.id ? styles.stripItemActive : ''}`}
              aria-label={cove.name}
              title={cove.name}
              aria-current={routeParamFromPath(currentPath, '/cove/') === cove.id ? 'page' : undefined}
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
                <Icon name="plus" />
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
                    coveWaves={visibleWaves(wavesByCove.get(cove.id) ?? [])}
                    expanded={expandedOverride.get(cove.id) ?? true}
                    onToggle={(next) => setExpandedOverride((current) => new Map(current).set(cove.id, next))}
                    onRequestDelete={coveConfirm.request}
                    onNewWave={onNewWave}
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
              /*
               * Two destinations and the way out. The theme cycler that used to
               * sit on top is gone: it was the only item here that *did*
               * something instead of taking you somewhere, it cycled blind
               * through three modes with the result off-screen behind the
               * menu, and Settings › General states the same preference as
               * three labelled options you can see the effect of.
               */
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
        open={waveConfirm.open}
        title={DELETE_WAVE_COPY.title}
        description={DELETE_WAVE_COPY.description}
        confirmLabel={DELETE_WAVE_COPY.confirmLabel}
        confirmBusyLabel="Deleting…"
        confirmState={waveConfirm.pending ? 'busy' : 'ready'}
        onConfirm={waveConfirm.confirm}
        onCancel={waveConfirm.cancel}
      />
      <OperationFeedback feedback={waveConfirm.feedback} />
      <OperationFeedback feedback={coveConfirm.feedback} />
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
/**
 * "Waiting on you" and "Pinned" — the two shortcut sections.
 *
 * Their rows are **never marked current**, and that is the one thing worth
 * saying about them. A wave that is open, pinned, and waiting used to light up
 * three times in one 200px column, which does not tell you where you are three
 * times as well — it tells you three different places are where you are. These
 * sections are shortcuts *into* the tree; the tree is where a location is
 * shown, and the cove list is the tree. One place to look, and it is the one
 * that also says which cove the wave belongs to.
 */
function WaveSection({ title, waves, coves, onGo, nowMs, onSetPinned, onDelete }: RowProps & {
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
            onOpen={(waveId) => onGo({ name: 'wave', waveId })}
            onSetPinned={onSetPinned}
            onDelete={onDelete}
          />
        ))}
      </div>
    </div>
  );
}

/**
 * INV-A11Y-061 — navigation is `<button>` + `onGo`, never a native `<a href>`.
 * That holds for the `+` too: it opens a dialog, so it is a button and a
 * callback, not a link to a "new wave" URL that does not exist.
 *
 * INV-SIDEBAR-013 — the row carries **two** trailing controls, and only one of
 * them is hover-revealed. The `+` is permanent because starting a wave is the
 * rail's one creative action and a control you have to discover by hovering is
 * a control most people never find; the `×` stays revealed because a
 * cove-deleting button permanently on every row is a row of loaded guns.
 *
 * They do not share a slot, and neither ever moves: `+` sits at the trailing
 * edge, `×` one control-step inboard, and the row reserves both gutters at
 * rest. The wave row's status dot/delete pair *does* share one slot — that
 * works because the two marks are the same size and mean the same place. Two
 * live buttons cannot do that, so this row spends the second 20px instead.
 */
function CoveGroup({
  cove, coveWaves, expanded, onToggle, onRequestDelete, onNewWave,
  currentPath, onGo, nowMs, onSetPinned, onDelete,
}: RowProps & {
  cove: Cove;
  coveWaves: readonly Wave[];
  expanded: boolean;
  onToggle: (expanded: boolean) => void;
  onRequestDelete: (coveId: string) => void;
  onNewWave: (coveId: string) => void;
}) {
  const active = routeParamFromPath(currentPath, '/cove/') === cove.id;

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
          <Icon name="chevron-right" />
        </button>
        <button
          type="button"
          data-nc-role="icon"
          className={styles.coveDelete}
          aria-label={`Delete cove ${cove.name}`}
          onClick={() => onRequestDelete(cove.id)}
        >
          <Icon name="close" size="sm" />
        </button>
        {/* The accessible name names the cove, and it has to: the rail now
            carries one of these per cove, and N controls all called "New wave"
            is a list a screen-reader user cannot choose from. `title` is the
            sighted hover label — §4.4 requires both, because a tooltip may not
            stand in for the accessible name. */}
        <button
          type="button"
          data-nc-role="icon"
          className={styles.coveNew}
          aria-label={`New wave in ${cove.name}`}
          title="New wave"
          onClick={() => onNewWave(cove.id)}
        >
          <Icon name="plus" size="sm" />
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
              active={routeParamFromPath(currentPath, '/wave/') === wave.id}
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
