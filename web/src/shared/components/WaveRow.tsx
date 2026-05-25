import type { Cove, Wave } from '../../types';
import { isRunning } from '../lifecycle';
import { CloseIcon } from './CloseIcon';
import { PinIcon } from './PinIcon';
import { ProgressBar } from './ProgressBar';
import { WaveGlyph } from './WaveGlyph';
import { WaveLifecycleBadge } from './WaveLifecycleBadge';

// ---------------- WaveRow ----------------

export function WaveRow({
  wave,
  cove,
  showCove = true,
  onClick,
  onDelete,
  onPinWave,
}: {
  wave: Wave;
  cove?: Cove;
  showCove?: boolean;
  onClick?: () => void;
  /** Optional per-row delete. When supplied, a × button reveals on hover
   *  on the right of the row. Caller is responsible for its own confirm
   *  dialog (so the row delete and header delete read identically). */
  onDelete?: () => void;
  /** Optional pin/unpin. When supplied, a hover-revealed pin button appears
   *  on the row — always visible when the wave is already pinned so unpin
   *  is discoverable on touch. Mirrors the sidebar's WaveRow pin button. */
  onPinWave?: (waveId: string, pin: boolean) => void | Promise<void>;
}) {
  // Avoid the "double-bullet" effect: only emit the `·` separator when both
  // a cove tag AND a `now` line are going to render. Empty `now` (i.e. no
  // plugin posted activity text) drops out cleanly.
  const showCoveTag = showCove && !!cove;
  const showNow = !!wave.now;
  const showEta = !!wave.eta;
  const running = isRunning(wave.lifecycle);
  const showProgress = running && wave.progress > 0;
  const pinned = wave.pinnedAt != null;

  // The row is a real <button> so Enter/Space activation and focus
  // semantics come for free. The hover-reveal × delete is a SIBLING
  // <button> (NOT nested) inside a positioning wrapper — nesting buttons
  // is invalid HTML and trips axe's `nested-interactive`. The wrapper
  // owns `position: relative` so the absolutely-positioned delete can
  // sit on top of the row; CSS rules out it as a visible overlap by
  // reserving a 32px right gutter inside `.wave-row` and hover/focus-
  // within on the wrapper controls the reveal. When `onDelete` is
  // absent the row stands alone — same wrapper, no sibling.
  //
  // When `onClick` is undefined the row is rendered as a non-clickable
  // <button disabled> so its visual treatment is unchanged but it isn't
  // activatable; the existing call sites always pass an onClick, so this
  // path is mostly defensive (e.g. read-only embedded views).
  return (
    <div className="wave-row-wrapper">
      <button
        type="button"
        className="wave-row"
        onClick={onClick}
        disabled={!onClick}
      >
        <WaveGlyph lifecycle={wave.lifecycle} />
        <div className="body">
          <div className="t">{wave.title}</div>
          {(showCoveTag || showNow) && (
            <div className="s">
              {showCoveTag && (
                <span className="cove-tag">
                  <i style={{ background: cove!.color }} />
                  {cove!.name}
                </span>
              )}
              {showCoveTag && showNow && <span>·</span>}
              {showNow && <span>{wave.now}</span>}
            </div>
          )}
          {/* Issue #145 — secondary lifecycle pill on the wave row.
              `compact` skips the leading dot so we don't double up
              with the row's own status glyph on the left. The badge
              shows up regardless of `now`/`cove` so the row's
              lifecycle is always visible (the only "always present"
              wave-level state). */}
          <div className="s">
            <WaveLifecycleBadge lifecycle={wave.lifecycle} compact />
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
            {showProgress && <ProgressBar value={wave.progress} running />}
            {showEta && <span className="when">{wave.eta}</span>}
          </div>
        )}
      </button>
      {onDelete && (
        <button
          type="button"
          className="wave-row-delete"
          onClick={(e) => {
            e.stopPropagation();
            onDelete();
          }}
          title={`Delete "${wave.title}"`}
          aria-label={`Delete "${wave.title}"`}
        >
          <CloseIcon />
        </button>
      )}
      {onPinWave && (
        <button
          type="button"
          className={'side-wave-pin' + (pinned ? ' pinned' : '')}
          onClick={(e) => {
            e.stopPropagation();
            void onPinWave(wave.id, !pinned);
          }}
          aria-label={pinned ? 'Unpin wave' : 'Pin wave'}
        >
          <PinIcon down={pinned} />
        </button>
      )}
    </div>
  );
}
