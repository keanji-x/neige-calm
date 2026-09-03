// INV-DUP-009 — the one track row.
//
// Two surfaces render it: the sidebar rail and Today.
// They must agree on what a track *looks* like, so the row is declared once here
// and consumed through this entry.
//
// It stays under `features/track` and not in `ui/`: `ui/` is deliberately
// domain-free (it may import core *types* only), and this row reads lifecycle
// predicates and display rules. Today receives it by injection instead —
// `app/router` hands it a `renderTrackRow` callback. `app/**` may import
// features; siblings may not.
//
// §6.3 gives it four variants. They differ in height, in what the leading 6px
// column carries, and in whether the second line exists at all — the compact,
// panel and rail variants *drop* the lifecycle line rather than shrinking it,
// so a lifecycle phrase simply does not exist on those surfaces.

import {
  isRunning, lifecycleLabel, needsUserAttention, trackDisplayTitle, type Track,
} from '../../../../../core/domain/track.ts';
import { Icon } from '../../../ui/icon/public.tsx';
import styles from './row.module.css';

export type TrackRowVariant = 'default' | 'compact' | 'panel' | 'rail';

export type TrackRowProps = Readonly<{
  track: Track;
  variant?: TrackRowVariant;
  /** Area name, when the surface does not already group by area. */
  areaName?: string;
  /** Agenda rows only: the hour bucket of a `ScheduledEvent`. */
  hourLabel?: string;
  active?: boolean;
  /** Pins "now" so relative times cannot drift between render and assertion. */
  nowMs?: number;
  onOpen: (trackId: string) => void;
  /** Supplying this reveals a pin button. See INV-SIDEBAR-012 below. */
  onSetPinned?: (trackId: string, pinned: boolean) => void;
  /** Supplying this reveals a delete button. The caller owns the confirm. */
  onDelete?: (trackId: string) => void;
}>;

function variantClass(variant: TrackRowVariant): string {
  switch (variant) {
    case 'default': return styles.variantDefault;
    case 'compact': return styles.variantCompact;
    case 'panel': return styles.variantPanel;
    case 'rail': return styles.variantRail;
  }
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/**
 * §2.2's relative-time rule, floored and lower-case. Past 30 days it becomes an
 * absolute date: "5w" stops being a duration anyone can picture.
 */
export function relativeTime(atMs: number, nowMs: number): string {
  const elapsed = Math.max(0, nowMs - atMs);
  if (elapsed >= 30 * DAY) {
    return new Date(atMs).toLocaleDateString('en-US', { month: 'short', day: 'numeric' });
  }
  if (elapsed >= 7 * DAY) return `${Math.floor(elapsed / (7 * DAY))}w`;
  if (elapsed >= DAY) return `${Math.floor(elapsed / DAY)}d`;
  if (elapsed >= HOUR) return `${Math.floor(elapsed / HOUR)}h`;
  if (elapsed >= MINUTE) return `${Math.floor(elapsed / MINUTE)}m`;
  return 'now';
}

/**
 * The row is a `<button>` and the pin/delete affordances are **siblings**, not
 * children: nesting interactive elements is invalid HTML and trips axe's
 * `nested-interactive`. A positioning wrapper owns the hover reveal, and the
 * row reserves a right gutter so the buttons never overlap the title.
 *
 * INV-SIDEBAR-012 — the pin button is hover-revealed **except when the track is
 * already pinned**, where it stays permanently visible. Touch has no hover, so
 * a hover-only unpin would be unreachable on a tablet: once pinned, the only
 * way back out has to be visible.
 *
 * INV-A11Y-061 — navigation is this button plus `onOpen`, never an `<a href>`.
 */
export function TrackRow({
  track, variant = 'default', areaName, hourLabel, active = false, nowMs,
  onOpen, onSetPinned, onDelete,
}: TrackRowProps) {
  const attention = needsUserAttention(track);
  const running = isRunning(track.lifecycle);
  const pinned = track.pinnedAt !== null;
  const title = trackDisplayTitle(track.title);
  const lifecycle = lifecycleLabel(track.lifecycle);
  const hasPin = onSetPinned !== undefined;
  const hasRemove = onDelete !== undefined;
  const now = nowMs ?? Date.now();

  const bits = [attention ? 'waiting on you' : '', running ? 'running' : ''].filter(Boolean);
  const label = `Track ${title}${bits.length > 0 ? `, ${bits.join(', ')}` : ''}, ${lifecycle}`
    + (areaName === undefined ? '' : `, in area ${areaName}`);

  const dotClass = `${styles.dot} ${attention ? styles.dotWaiting : running ? styles.dotRunning : ''}`;

  /*
   * The rail moves the status dot to the *trailing* edge and lets the delete
   * take its place on hover. §3.1 already assigns that edge to status ("右边缘
   * = 状态"), so this is the row finally obeying it — and it buys three things
   * at once: the title starts 10px further left (116px of text in a 200px rail
   * became 126px), the area row stops having to reserve an empty 6px cell for a
   * status it does not have, and the trailing zone holds exactly one thing at a
   * time instead of two.
   *
   * Losing the dot on the row you are pointing at is the cost, and it is small:
   * hover is transient, it is one row, and the lifecycle is still in the row's
   * accessible name. The dot is `aria-hidden` decoration either way, which is
   * what lets it move outside the button at all.
   *
   * The panel variant does the same, for the same reason and one more: a card
   * module is 308px wide and its head already carries a `+` on that edge, so
   * the row's status and its delete land in the column the module head started.
   * The relative time goes with it — in a 308px column an age was competing
   * with the title for the width the title needed, and a track's page states it
   * properly.
   *
   * `default` keeps the leading dot. Its trailing edge is already spent on the
   * relative time, and a 720px row does not need the 10px.
   */
  const trailingStatus = variant === 'rail' || variant === 'panel';

  return (
    <div className={[
      styles.wrapper,
      variant === 'rail' ? styles.wrapperRail : '',
      variant === 'panel' ? styles.wrapperPanel : '',
    ].filter(Boolean).join(' ')}>
      <button
        type="button"
        data-nc-role="row"
        className={[
          styles.row, variantClass(variant),
          active ? styles.rowActive : '',
          hasPin ? styles.hasPin : '', hasRemove ? styles.hasRemove : '',
        ].filter(Boolean).join(' ')}
        aria-current={active ? 'page' : undefined}
        aria-label={label}
        onClick={() => onOpen(track.id)}
      >
        {!trailingStatus && (
          <span
            className={dotClass}
            aria-hidden="true"
          />
        )}
        <span className={styles.titleRow}>
          {hourLabel !== undefined && <span className={styles.hour}>{hourLabel}</span>}
          <span className={styles.title} title={title}>{title}</span>
        </span>
        {/* The agenda is grouped by date already, so a relative time there is
            restating the heading in a 308px column. */}
        {!trailingStatus && (
          <span className={styles.age}>{relativeTime(track.updatedAt, now)}</span>
        )}
        {variant === 'default' && (
          <span className={styles.lifecycle}>{lifecycle}</span>
        )}
      </button>

      {trailingStatus && <span className={`${dotClass} ${styles.statusSlot}`} aria-hidden="true" />}
      {onSetPinned !== undefined && (
        <button
          type="button"
          data-nc-role="icon"
          className={`${styles.action} ${styles.pin} ${pinned ? styles.pinOn : ''}`}
          aria-label={pinned ? `Unpin ${title}` : `Pin ${title}`}
          aria-pressed={pinned}
          onClick={() => onSetPinned(track.id, !pinned)}
        >
          {/*
            The shared arrow stays stable across both states. The accessible
            name carries Pin versus Unpin, while aria-pressed and §4.5's
            permanent visibility carry the toggle state without introducing a
            second icon family or a hollow/solid pair.
          */}
          <Icon name="arrow-up" size="sm" />
        </button>
      )}
      {onDelete !== undefined && (
        <button
          type="button"
          data-nc-role="icon"
          className={`${styles.action} ${styles.remove}`}
          aria-label={`Delete ${title}`}
          onClick={() => onDelete(track.id)}
        >
          <Icon name="close" size="sm" />
        </button>
      )}
    </div>
  );
}
