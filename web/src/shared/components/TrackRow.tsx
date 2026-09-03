import type { Area, Track } from '../../types';
import { isRunning } from '../lifecycle';
import { CloseIcon } from './CloseIcon';
import { PinIcon } from './PinIcon';
import { ProgressBar } from './ProgressBar';
import { TrackGlyph } from './TrackGlyph';
import { TrackLifecycleBadge } from './TrackLifecycleBadge';
import { trackDisplayTitle } from '../trackTitle';

// ---------------- TrackRow ----------------

export function TrackRow({
  track,
  area,
  showArea = true,
  onClick,
  onDelete,
  onPinTrack,
}: {
  track: Track;
  area?: Area;
  showArea?: boolean;
  onClick?: () => void;
  /** Optional per-row delete. When supplied, a × button reveals on hover
   *  on the right of the row. Caller is responsible for its own confirm
   *  dialog (so the row delete and header delete read identically). */
  onDelete?: () => void;
  /** Optional pin/unpin. When supplied, a hover-revealed pin button appears
   *  on the row — always visible when the track is already pinned so unpin
   *  is discoverable on touch. Mirrors the sidebar's TrackRow pin button. */
  onPinTrack?: (trackId: string, pin: boolean) => void | Promise<void>;
}) {
  // Avoid the "double-bullet" effect: only emit the `·` separator when both
  // an area tag AND a `now` line are going to render. Empty `now` (i.e. no
  // plugin posted activity text) drops out cleanly.
  const showAreaTag = showArea && !!area;
  const showNow = !!track.now;
  const showEta = !!track.eta;
  const running = isRunning(track.lifecycle);
  const showProgress = running && track.progress > 0;
  const pinned = track.pinnedAt != null;
  const displayTitle = trackDisplayTitle(track.title);

  // The row is a real <button> so Enter/Space activation and focus
  // semantics come for free. The hover-reveal × delete is a SIBLING
  // <button> (NOT nested) inside a positioning wrapper — nesting buttons
  // is invalid HTML and trips axe's `nested-interactive`. The wrapper
  // owns `position: relative` so the absolutely-positioned delete can
  // sit on top of the row; CSS rules out it as a visible overlap by
  // reserving a 32px right gutter inside `.track-row` and hover/focus-
  // within on the wrapper controls the reveal. When `onDelete` is
  // absent the row stands alone — same wrapper, no sibling.
  //
  // When `onClick` is undefined the row is rendered as a non-clickable
  // <button disabled> so its visual treatment is unchanged but it isn't
  // activatable; the existing call sites always pass an onClick, so this
  // path is mostly defensive (e.g. read-only embedded views).
  return (
    <div className="track-row-wrapper">
      <button
        type="button"
        className="track-row"
        onClick={onClick}
        disabled={!onClick}
      >
        <TrackGlyph lifecycle={track.lifecycle} />
        <div className="body">
          <div className="t">{displayTitle}</div>
          {(showAreaTag || showNow) && (
            <div className="s">
              {showAreaTag && (
                <span className="area-tag">
                  <i style={{ background: area!.color }} />
                  {area!.name}
                </span>
              )}
              {showAreaTag && showNow && <span>·</span>}
              {showNow && <span>{track.now}</span>}
            </div>
          )}
          {/* Issue #145 — secondary lifecycle pill on the track row.
              `compact` skips the leading dot so we don't double up
              with the row's own status glyph on the left. The badge
              shows up regardless of `now`/`area` so the row's
              lifecycle is always visible (the only "always present"
              track-level state). */}
          <div className="s">
            <TrackLifecycleBadge lifecycle={track.lifecycle} compact />
          </div>
        </div>
        {(showProgress || showEta) && (
          <div
            style={{
              display: 'flex',
              flexDirection: 'column',
              alignItems: 'flex-end',
              gap: 6,
              minWidth: 110,
            }}
          >
            {showProgress && <ProgressBar value={track.progress} running />}
            {showEta && <span className="when">{track.eta}</span>}
          </div>
        )}
      </button>
      {onDelete && (
        <button
          type="button"
          className="track-row-delete"
          onClick={(e) => {
            e.stopPropagation();
            onDelete();
          }}
          title={`Delete "${displayTitle}"`}
          aria-label={`Delete "${displayTitle}"`}
        >
          <CloseIcon />
        </button>
      )}
      {onPinTrack && (
        <button
          type="button"
          className={'side-track-pin' + (pinned ? ' pinned' : '')}
          onClick={(e) => {
            e.stopPropagation();
            void onPinTrack(track.id, !pinned);
          }}
          aria-label={pinned ? 'Unpin track' : 'Pin track'}
        >
          <PinIcon down={pinned} />
        </button>
      )}
    </div>
  );
}
