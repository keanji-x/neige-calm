// INV-DUP-009 — the one wave row.
//
// Three surfaces render it: the sidebar rail, the cove page list, and Today.
// They must agree on what a wave *looks* like, so the row is declared once here
// and consumed through this entry.
//
// It stays under `features/wave` and not in `ui/`: `ui/` is deliberately
// domain-free (it may import core *types* only), and this row reads lifecycle
// predicates and display rules. The two surfaces outside this domain receive it
// by injection instead — `app/router` composes the cove page's list, and hands
// Today a `renderWaveRow` callback. `app/**` may import features; siblings may not.
//
// §6.3 gives it four variants. They differ in height, in what the leading 6px
// column carries, and in whether the second line exists at all — the compact,
// agenda and rail variants *drop* the lifecycle line rather than shrinking it,
// so a lifecycle phrase simply does not exist on those surfaces.

import {
  isRunning, lifecycleLabel, needsUserAttention, waveDisplayTitle, type Wave,
} from '../../../../../core/domain/wave.ts';
import { coveSlotVar } from '../../../../../core/domain/cove.ts';
import styles from './row.module.css';

export type WaveRowVariant = 'default' | 'compact' | 'agenda' | 'rail';

export type WaveRowProps = Readonly<{
  wave: Wave;
  variant?: WaveRowVariant;
  /** Cove name, when the surface does not already group by cove. */
  coveName?: string;
  /** Agenda rows only: the hour bucket of a `ScheduledEvent`. */
  hourLabel?: string;
  active?: boolean;
  /** Pins "now" so relative times cannot drift between render and assertion. */
  nowMs?: number;
  onOpen: (waveId: string) => void;
  /** Supplying this reveals a pin button. See INV-SIDEBAR-012 below. */
  onSetPinned?: (waveId: string, pinned: boolean) => void;
  /** Supplying this reveals a delete button. The caller owns the confirm. */
  onDelete?: (waveId: string) => void;
}>;

function variantClass(variant: WaveRowVariant): string {
  switch (variant) {
    case 'default': return styles.variantDefault;
    case 'compact': return styles.variantCompact;
    case 'agenda': return styles.variantAgenda;
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
 * INV-SIDEBAR-012 — the pin button is hover-revealed **except when the wave is
 * already pinned**, where it stays permanently visible. Touch has no hover, so
 * a hover-only unpin would be unreachable on a tablet: once pinned, the only
 * way back out has to be visible.
 *
 * INV-A11Y-061 — navigation is this button plus `onOpen`, never an `<a href>`.
 */
export function WaveRow({
  wave, variant = 'default', coveName, hourLabel, active = false, nowMs,
  onOpen, onSetPinned, onDelete,
}: WaveRowProps) {
  const attention = needsUserAttention(wave);
  const running = isRunning(wave.lifecycle);
  const pinned = wave.pinnedAt !== null;
  const title = waveDisplayTitle(wave.title);
  const lifecycle = lifecycleLabel(wave.lifecycle);
  const hasPin = onSetPinned !== undefined;
  const hasRemove = onDelete !== undefined;
  const now = nowMs ?? Date.now();

  // The agenda column spans coves and is not split by status, so its leading
  // column answers "whose is this?" instead. Every other surface is already
  // grouped by cove, or sectioned by status, and uses a status dot.
  const identity = variant === 'agenda';

  const bits = [attention ? 'waiting on you' : '', running ? 'running' : ''].filter(Boolean);
  const label = `Wave ${title}${bits.length > 0 ? `, ${bits.join(', ')}` : ''}, ${lifecycle}`
    + (coveName === undefined ? '' : `, in cove ${coveName}`);

  const dotClass = identity
    ? styles.dot
    : `${styles.dot} ${attention ? styles.dotWaiting : running ? styles.dotRunning : ''}`;

  /*
   * The rail moves the status dot to the *trailing* edge and lets the delete
   * take its place on hover. §3.1 already assigns that edge to status ("右边缘
   * = 状态"), so this is the row finally obeying it — and it buys three things
   * at once: the title starts 10px further left (116px of text in a 200px rail
   * became 126px), the cove row stops having to reserve an empty 6px cell for a
   * status it does not have, and the trailing zone holds exactly one thing at a
   * time instead of two.
   *
   * Losing the dot on the row you are pointing at is the cost, and it is small:
   * hover is transient, it is one row, and the lifecycle is still in the row's
   * accessible name. The dot is `aria-hidden` decoration either way, which is
   * what lets it move outside the button at all.
   *
   * The other three variants keep the leading dot. Their trailing edge is
   * already spent on the relative time, and a 720px row does not need the 10px.
   */
  const railStatus = variant === 'rail';

  return (
    <div className={`${styles.wrapper} ${railStatus ? styles.wrapperRail : ''}`}>
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
        onClick={() => onOpen(wave.id)}
      >
        {!railStatus && (
          <span
            className={dotClass}
            style={identity ? { background: `var(${coveSlotVar(wave.coveId)})` } : undefined}
            aria-hidden="true"
          />
        )}
        <span className={styles.titleRow}>
          {hourLabel !== undefined && <span className={styles.hour}>{hourLabel}</span>}
          <span className={styles.title} title={title}>{title}</span>
        </span>
        {/* The agenda is grouped by date already, so a relative time there is
            restating the heading in a 308px column. */}
        {variant !== 'agenda' && variant !== 'rail' && (
          <span className={styles.age}>{relativeTime(wave.updatedAt, now)}</span>
        )}
        {variant === 'default' && (
          <span className={styles.lifecycle}>{lifecycle}</span>
        )}
      </button>

      {railStatus && <span className={`${dotClass} ${styles.statusSlot}`} aria-hidden="true" />}
      {onSetPinned !== undefined && (
        <button
          type="button"
          data-nc-role="icon"
          className={`${styles.action} ${styles.pin} ${pinned ? styles.pinOn : ''}`}
          aria-label={pinned ? `Unpin ${title}` : `Pin ${title}`}
          aria-pressed={pinned}
          onClick={() => onSetPinned(wave.id, !pinned)}
        >
          {/*
            The glyph names the *action*, not the state — so it flips. Unpinned
            offers ↑ ("lift this to the top"); pinned offers ↓ ("put it back"),
            which is the toggle's off-switch and the reason §4.5 keeps it
            visible at rest in the first place. Direction is the whole signal:
            still one stroke, still one weight, still greyscale — no
            hollow/solid pair, and no second channel.
          */}
          {pinned ? '↓' : '↑'}
        </button>
      )}
      {onDelete !== undefined && (
        <button
          type="button"
          data-nc-role="icon"
          className={`${styles.action} ${styles.remove}`}
          aria-label={`Delete ${title}`}
          onClick={() => onDelete(wave.id)}
        >
          ×
        </button>
      )}
    </div>
  );
}
