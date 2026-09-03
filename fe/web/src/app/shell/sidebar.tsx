// The workspace rail. Three sections in a fixed order; see INV-SIDEBAR-007.
//
// §7.5 — the rail has exactly one emphasis rule: only the current location may
// use `--accent`, and only "WAITING ON YOU" may show `--warn`. Everything else
// is greyscale. A sidebar with three colours of status is a status board, not
// navigation.

import { useEffect, useRef } from 'react';

import { areaOf, visibleAreas, type Area } from '../../../../core/domain/area.ts';
import { needsUserAttention, userVisibleTracks, visibleTracks, type Track } from '../../../../core/domain/track.ts';
import { AREA_PALETTE } from '../../features/area/palette.ts';
import { TrackRow } from '../../features/track/row/public.tsx';
import { deleteAreaCopy, DELETE_TRACK_COPY } from '../../ui/confirm-dialog/copy.ts';
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
  areas: readonly Area[];
  tracksByArea: ReadonlyMap<string, readonly Track[]>;
  tracks: readonly Track[];
  currentPath: string;
  onGo: (target: NavTarget) => void;
  /** Colour is picked here, at random from `AREA_PALETTE` (INV-DUP-006). */
  onCreateArea: (name: string, color: string) => void | Promise<void>;
  onRenameArea: (areaId: string, name: string) => void | Promise<void>;
  onDeleteArea: (areaId: string, signal: AbortSignal) => void | Promise<void>;
  /** Goes to the new-track page for this area. The rail does not own that
   *  page — it is the route `/area/{id}/new` (#1211) — and it reaches it
   *  through the shell, which is the nearest owner both `+` surfaces share. */
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

function randomAreaColor(): string {
  return AREA_PALETTE[Math.floor(Math.random() * AREA_PALETTE.length)] ?? AREA_PALETTE[0];
}

/**
 * INV-SIDEBAR-007 — the three sections render in this order, and **pinning is
 * not relocation**: a pinned track appears in the Pinned section *and* in its
 * area's inline list, and if it also needs attention it appears in "Waiting on
 * you" as well. Waiting deliberately includes pinned attention tracks.
 *
 * INV-A11Y-058 — there is **intentionally no skip-to-main link**. The rail is
 * short enough that it has not been raised as a pain point (a11y contract
 * §3.1/§9). If a second rail section with many rows lands, re-evaluate.
 *
 * INV-SIDEBAR-012 — every row's pin button is hover-revealed while the track is
 * unpinned and permanently visible once it is pinned; the reveal itself is CSS
 * (`features/track/row/row.module.css`), so a jsdom test can only prove the
 * control is always in the accessibility tree and carries `aria-pressed`.
 *
 * E2E-INV-SHELL-003 — `userVisibleTracks` filters the kernel system area here
 * as well as in the query layer, so scaffolding cannot reach the rail even if a
 * caller hands over an unfiltered list. It is the same function mobile Pages
 * uses, so the two surfaces cannot drift (#1191 §3.1).
 */
export function Sidebar({
  areas, tracksByArea, tracks, currentPath, onGo,
  onCreateArea, onRenameArea, onDeleteArea, onNewTrack, onSetPinned, onDeleteTrack,
  onOpenSettings, onOpenPlugins, onSignOut, collapsed, onToggleCollapsed,
  userLabel = 'You', nowMs, readError = null, activityError = null,
  readLoading = false, onRetryRead = () => undefined,
}: SidebarProps) {
  const [expandedOverride, setExpandedOverride] = useState<ReadonlyMap<string, boolean>>(() => new Map());
  const [creatingArea, setCreatingArea] = useState(false);
  const [areaDraft, setAreaDraft] = useState('');
  const areaInputRef = useRef<HTMLInputElement | null>(null);
  const railRef = useRef<HTMLElement | null>(null);
  const areaDisclosureRefs = useRef(new Map<string, HTMLButtonElement>());
  const pendingAreaFocusRef = useRef<string | null>(null);
  const trackConfirm = useDeleteConfirm(onDeleteTrack);
  const areaConfirm = useDeleteConfirm(onDeleteArea, () => onGo({ name: 'today' }));
  const writeFeedback = useOperationFeedback();

  const userAreas = visibleAreas(areas);
  const userTracks = userVisibleTracks(tracks, areas);
  const waiting = userTracks.filter(needsUserAttention);
  const pinned = userTracks.filter((track) => track.pinnedAt !== null)
    .toSorted((left, right) => (right.pinnedAt ?? 0) - (left.pinnedAt ?? 0));

  const activeTrackId = routeParamFromPath(currentPath, '/track/') ?? null;
  const activeAreaId = activeTrackId === null
    ? null
    : userTracks.find((track) => track.id === activeTrackId)?.areaId ?? null;

  const deletingArea = userAreas.find((area) => area.id === areaConfirm.target);
  const typed = useTypedConfirm(deletingArea?.name ?? '');
  const areaCopy = deleteAreaCopy(
    deletingArea?.name ?? '',
    tracksByArea.get(areaConfirm.target ?? '')?.length,
  );

  // Navigating into a track drops any manual collapse on its area — the row the
  // user just opened has to be visible. Dropping the override (rather than
  // forcing `true`) keeps the chevron usable straight afterwards.
  useEffect(() => {
    if (activeAreaId === null) return;
    setExpandedOverride((current) => {
      if (!current.has(activeAreaId)) return current;
      const next = new Map(current);
      next.delete(activeAreaId);
      return next;
    });
  }, [activeAreaId, activeTrackId]);

  /*
   * …and then brings it into view. Expanding the area is only half of "show me
   * where I am": a workspace with a dozen areas puts the open track below the
   * fold as often as not, and the rail then shows an expanded area with nothing
   * marked in it.
   *
   * The target is found by `aria-current="page"`, which is the same fact the
   * highlight is drawn from rather than a second copy of it — there is exactly
   * one such row now that the shortcut sections no longer claim to be current.
   *
   * `block: 'nearest'` scrolls only when the row is actually outside the
   * viewport, so arriving at a track already on screen moves nothing (principle
   * 3). It re-runs on `expandedOverride` too, because the effect above may have
   * only just expanded the area the row lives in.
   */
  useEffect(() => {
    if (collapsed || activeTrackId === null) return;
    railRef.current?.querySelector('[aria-current="page"]')
      ?.scrollIntoView?.({ block: 'nearest' });
  }, [activeTrackId, collapsed, expandedOverride]);

  /* A collapsed Area initial is an entrance into the expanded tree, not a
     destination of its own. Restore focus to the disclosure it reveals and
     bring that row into view; otherwise activating the unmounted initial drops
     keyboard focus onto <body>, and a long rail may reveal no trace of the Area
     the reader chose. This deliberately runs after the current-Track scroll
     above, so a Track in another Area cannot steal the final scroll position
     during the same collapsed → expanded commit. */
  useEffect(() => {
    if (collapsed) return;
    const areaId = pendingAreaFocusRef.current;
    if (areaId === null) return;
    pendingAreaFocusRef.current = null;
    const disclosure = areaDisclosureRefs.current.get(areaId);
    disclosure?.focus({ preventScroll: true });
    disclosure?.scrollIntoView?.({ block: 'nearest' });
  }, [collapsed]);

  useEffect(() => { if (creatingArea) areaInputRef.current?.focus(); }, [creatingArea]);

  const submitArea = () => {
    const name = areaDraft.trim();
    if (name === '') return;
    setCreatingArea(false);
    setAreaDraft('');
    void writeFeedback.run(Promise.resolve(onCreateArea(name, randomAreaColor())), 'Could not create the area.');
  };

  const rowProps = {
    currentPath,
    onGo,
    nowMs,
    onSetPinned: (trackId: string, next: boolean) => {
      void writeFeedback.run(Promise.resolve(onSetPinned(trackId, next)), 'Could not update the track.');
    },
    onDelete: trackConfirm.request,
  };

  // The rail has no area yet: the one remedy is to make one, so the input opens
  // in the first row's place rather than a sentence pointing at a button
  // elsewhere (§5.3). It is not auto-focused — that would steal the reading
  // position a screen-reader user just landed on.
  const showInlineCreate = readError === null && !readLoading && (creatingArea || userAreas.length === 0);

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
      {activityError !== null && <ErrorBox message={`Track activity is unavailable: ${activityError}`} onRetry={onRetryRead} />}
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
          {/* An initial, not a colour chip. Eight area hues stacked down a 44px
              strip turned navigation into a palette — and they were the app's
              only use of `--area-*` outside the surfaces that genuinely mix
              areas (Today's agenda, the calendar day dot). A letter says which
              area without spending a channel §7.5 reserves for state, and the
              current one is still marked the way every other row marks it:
              `--accent-soft` fill. */}
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
                setExpandedOverride((current) => new Map(current).set(area.id, true));
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
              <h2 className={styles.sectionTitle}>Areas</h2>
              <button
                type="button"
                data-nc-role="icon"
                className={styles.sectionAction}
                aria-label="New area"
                onClick={() => { setAreaDraft(''); setCreatingArea(true); }}
              >
                <Icon name="plus" />
              </button>
            </div>

            {showInlineCreate && (
              <input
                ref={areaInputRef}
                type="text"
                className={styles.inlineCreate}
                aria-label="Area name"
                placeholder="New area…"
                value={areaDraft}
                onChange={(event) => setAreaDraft(event.target.value)}
                /* §6.12 — an inline editor commits on blur, like the title
                   editor does. Clicking away from a field you have typed into
                   and losing the text is the behaviour people learn to distrust
                   inline editing for. Escape is the discard, and it clears the
                   draft first so this handler has nothing left to submit. */
                onBlur={submitArea}
                onKeyDown={(event) => {
                  if (event.key === 'Enter') { event.preventDefault(); submitArea(); }
                  else if (event.key === 'Escape') {
                    event.preventDefault();
                    setCreatingArea(false);
                    setAreaDraft('');
                  }
                }}
              />
            )}

            {userAreas.length > 0 && (
              <div className={styles.areaGroups}>
                {userAreas.map((area) => (
                  <AreaGroup
                    key={area.id}
                    area={area}
                    areaTracks={visibleTracks(tracksByArea.get(area.id) ?? [])}
                    expanded={expandedOverride.get(area.id) ?? true}
                    onToggle={(next) => setExpandedOverride((current) => new Map(current).set(area.id, next))}
                    disclosureRef={(element) => {
                      if (element === null) areaDisclosureRefs.current.delete(area.id);
                      else areaDisclosureRefs.current.set(area.id, element);
                    }}
                    onRename={(areaId, name) => {
                      void writeFeedback.run(
                        Promise.resolve(onRenameArea(areaId, name)),
                        'Could not rename the area.',
                      );
                    }}
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
      {/* Deleting an area cascades to every track inside it: the one operation in
          the product that earns a typed confirm (§4.3). */}
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
  currentPath: string;
  onGo: (target: NavTarget) => void;
  nowMs?: number;
  onSetPinned: (trackId: string, pinned: boolean) => void;
  onDelete: (trackId: string) => void;
}>;

/** A section with no rows does not render at all — no label, no dashed box.
 *  That absence is why the rail looks empty when nothing needs you (§6.1). */
/**
 * "Waiting on you" and "Pinned" — the two shortcut sections.
 *
 * Their rows are **never marked current**, and that is the one thing worth
 * saying about them. A track that is open, pinned, and waiting used to light up
 * three times in one 200px column, which does not tell you where you are three
 * times as well — it tells you three different places are where you are. These
 * sections are shortcuts *into* the tree; the tree is where a location is
 * shown, and the area list is the tree. One place to look, and it is the one
 * that also says which area the track belongs to.
 */
function TrackSection({ title, tracks, areas, onGo, nowMs, onSetPinned, onDelete }: RowProps & {
  title: string;
  tracks: readonly Track[];
  areas: readonly Area[];
}) {
  if (tracks.length === 0) return null;
  return (
    <div className={styles.section}>
      <h2 className={styles.sectionTitle}>{title}</h2>
      <div className={styles.sectionRows}>
        {tracks.map((track) => (
          <TrackRow
            key={track.id}
            track={track}
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
 * INV-A11Y-061 — navigation is `<button>` + `onGo`, never a native `<a href>`.
 * That holds for the `+` too, and since #1211 it is the *only* reason: the `+`
 * now goes to `/area/{id}/new`, so a real URL does exist and an `<a href>`
 * would work. It stays a button because this rail does not mix the two
 * activation models — see the rule above. The cost is real and worth naming:
 * middle-click, open-in-new-tab and copy-link do not work on it.
 *
 * INV-SIDEBAR-013 — the row carries **two** trailing controls, and only delete
 * is hover-revealed. The `+` is permanent because starting a Track
 * is the rail's one creative action and a control you have to discover by
 * hovering is a control most people never find.
 *
 * They do not share a slot, and neither ever moves: `+` sits at the trailing
 * edge, delete one control-step inboard, and the row reserves both gutters at
 * rest. The track row's status dot/delete pair *does* share one slot — that
 * works because the two marks are the same size and mean the same place. Two
 * live buttons cannot do that, so this row spends the second 20px instead.
 */
function AreaGroup({
  area, areaTracks, expanded, onToggle, disclosureRef, onRename, onRequestDelete, onNewTrack,
  currentPath, onGo, nowMs, onSetPinned, onDelete,
}: RowProps & {
  area: Area;
  areaTracks: readonly Track[];
  expanded: boolean;
  onToggle: (expanded: boolean) => void;
  disclosureRef: (element: HTMLButtonElement | null) => void;
  onRename: (areaId: string, name: string) => void;
  onRequestDelete: (areaId: string) => void;
  onNewTrack: (areaId: string) => void;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(area.name);
  const inputRef = useRef<HTMLInputElement | null>(null);
  const localDisclosureRef = useRef<HTMLButtonElement | null>(null);
  const restoreDisclosureFocusRef = useRef(false);

  useEffect(() => {
    if (editing) {
      inputRef.current?.focus();
      inputRef.current?.select();
      return;
    }
    if (!restoreDisclosureFocusRef.current) return;
    restoreDisclosureFocusRef.current = false;
    localDisclosureRef.current?.focus();
  }, [editing]);

  const beginRename = () => {
    setDraft(area.name);
    setEditing(true);
  };
  const commitRename = (restoreFocus = false) => {
    const name = draft.trim();
    restoreDisclosureFocusRef.current = restoreFocus;
    setEditing(false);
    if (name !== '' && name !== area.name) onRename(area.id, name);
  };

  return (
    <div className={styles.areaGroup}>
      <div className={styles.areaRowWrap}>
        {editing ? (
          <input
            ref={inputRef}
            className={styles.areaRenameInput}
            aria-label={`Rename area ${area.name}`}
            value={draft}
            onChange={(event) => setDraft(event.target.value)}
            onBlur={() => commitRename()}
            onKeyDown={(event) => {
              if (event.key === 'Enter') { event.preventDefault(); commitRename(true); }
              else if (event.key === 'Escape') {
                event.preventDefault();
                restoreDisclosureFocusRef.current = true;
                setDraft(area.name);
                setEditing(false);
              }
            }}
          />
        ) : (
          <button
            ref={(element) => {
              localDisclosureRef.current = element;
              disclosureRef(element);
            }}
            type="button"
            data-nc-role="row"
            className={styles.areaRow}
            aria-expanded={expanded}
            aria-label={`${expanded ? 'Collapse' : 'Expand'} area ${area.name}`}
            onClick={() => onToggle(!expanded)}
            onDoubleClick={beginRename}
            onKeyDown={(event) => {
              if (event.key === 'F2') { event.preventDefault(); beginRename(); }
            }}
          >
            <span className={`${styles.chevron} ${expanded ? styles.chevronOpen : ''}`} aria-hidden="true">
              <Icon name="chevron-right" />
            </span>
            <span className={styles.areaName} title={area.name}>{area.name}</span>
          </button>
        )}
        <button
          type="button"
          data-nc-role="icon"
          className={styles.areaDelete}
          aria-label={`Delete area ${area.name}`}
          title="Delete area"
          onClick={() => onRequestDelete(area.id)}
        >
          <Icon name="close" size="sm" />
        </button>
        {/* The accessible name names the area, and it has to: the rail now
            carries one of these per area, and N controls all called "New track"
            is a list a screen-reader user cannot choose from. `title` is the
            sighted hover label — §4.4 requires both, because a tooltip may not
            stand in for the accessible name. */}
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
      {expanded && (
        <div className={styles.trackList}>
          {areaTracks.map((track) => (
            <TrackRow
              key={track.id}
              track={track}
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
