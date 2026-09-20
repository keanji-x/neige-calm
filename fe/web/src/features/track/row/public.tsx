// The one track row, rendered by the sidebar rail and Today. It stays under `features/track` because it reads lifecycle predicates, which `ui/` may not.

import { ListText } from '../../../ui/list-typography/public.tsx';
import { useId } from 'react';
import { activityLabelOf, activityNameBit } from '../../../../../core/domain/activity.ts';
import {
  lifecycleLabel, trackActivityState, trackDisplayTitle, type Track,
} from '../../../../../core/domain/track.ts';
import { ActivityIndicator } from '../../../ui/activity-indicator/public.tsx';
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
  unread?: boolean;
  /** Pins "now" so relative times cannot drift between render and assertion. */
  nowMs?: number;
  onOpen: (trackId: string) => void;
  /** Supplying this reveals a pin button; once pinned it stays permanently visible, because touch has no hover. */
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

/** Relative time, floored and lower-case. Past 30 days it becomes an absolute date. */
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

/** The row is a `<button>` and the pin/delete affordances are siblings, not children: nesting interactive elements is invalid HTML. The pin stays visible once pinned, because touch has no hover. */
export function TrackRow({
  track, variant = 'default', areaName, hourLabel, active = false, nowMs,
  onOpen, onSetPinned, onDelete, unread = false,
}: TrackRowProps) {
  const descriptionId = useId();
  const pinned = track.pinnedAt !== null;
  const title = trackDisplayTitle(track.title);
  const lifecycle = lifecycleLabel(track.lifecycle);
  const hasPin = onSetPinned !== undefined;
  const hasRemove = onDelete !== undefined;
  const now = nowMs ?? Date.now();

  /* One state from the kernel's activity overlay plus the reader's receipt; the name's activity bit derives from the same value as the dot, never from the lifecycle. `unread` is the button's description, and only in the folded state. */
  const activity = trackActivityState(track, unread);
  const activityBit = activityNameBit(activity);
  const label = `Track ${title}${activityBit ? `, ${activityBit}` : ''}, ${lifecycle}`
    + (areaName === undefined ? '' : `, in area ${areaName}`);

  /* The rail and panel variants move the status dot to the trailing edge and let the delete take its place on hover; the dot is `aria-hidden` decoration either way. */
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
        aria-describedby={activity === 'unread' ? descriptionId : undefined}
        onClick={() => onOpen(track.id)}
      >
        {!trailingStatus && (
          <span className={styles.leadingStatus} aria-hidden="true"><ActivityIndicator state={activity} /></span>
        )}
        <span className={styles.titleRow}>
          {hourLabel !== undefined && <span className={styles.hour}>{hourLabel}</span>}
          <ListText tone="primary" emphasis={active ? 'selected' : variant === 'default' ? 'medium' : undefined}
            className={styles.title} title={title}>{title}</ListText>
        </span>
        {!trailingStatus && (
          <span className={styles.age}>{relativeTime(track.updatedAt, now)}</span>
        )}
        {variant === 'default' && (
          <span className={styles.lifecycle}>{lifecycle}</span>
        )}
      </button>

      {activity === 'unread' && <span hidden id={descriptionId}>{activityLabelOf('unread')}</span>}
      {trailingStatus && activity !== 'quiet' && <span className={styles.statusSlot} aria-hidden="true">
        <ActivityIndicator state={activity} />
      </span>}
      {onSetPinned !== undefined && (
        <button
          type="button"
          data-nc-role="icon"
          className={`${styles.action} ${styles.pin} ${pinned ? styles.pinOn : ''}`}
          aria-label={pinned ? `Unpin ${title}` : `Pin ${title}`}
          aria-pressed={pinned}
          onClick={() => onSetPinned(track.id, !pinned)}
        >
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
