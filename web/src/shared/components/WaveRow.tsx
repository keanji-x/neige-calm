import type { Cove, Wave } from '../../types';
import { ProgressBar } from './ProgressBar';
import { WaveGlyph } from './WaveGlyph';

// ---------------- WaveRow ----------------

export function WaveRow({
  wave,
  cove,
  showCove = true,
  onClick,
  onDelete,
}: {
  wave: Wave;
  cove?: Cove;
  showCove?: boolean;
  onClick?: () => void;
  /** Optional per-row delete. When supplied, a × button reveals on hover
   *  on the right of the row. Caller is responsible for its own confirm
   *  dialog (so the row delete and header delete read identically). */
  onDelete?: () => void;
}) {
  // Avoid the "double-bullet" effect: only emit the `·` separator when both
  // a cove tag AND a `now` line are going to render. Empty `now` (i.e. no
  // plugin posted activity text) drops out cleanly.
  const showCoveTag = showCove && !!cove;
  const showNow = !!wave.now;
  const showEta = !!wave.eta;
  const showProgress = wave.status === 'running' && wave.progress > 0;

  // The row used to be a real <button>, but adding a nested button for the
  // hover-reveal delete is invalid HTML. So the row is a div with the
  // navigation as a click+keydown handler, and the × is a real button
  // child whose click stops propagation so it doesn't also navigate.
  return (
    <div
      className="wave-row"
      onClick={onClick}
      role={onClick ? 'button' : undefined}
      tabIndex={onClick ? 0 : undefined}
      onKeyDown={(e) => {
        if (!onClick) return;
        if (e.key === 'Enter' || e.key === ' ') {
          e.preventDefault();
          onClick();
        }
      }}
    >
      <WaveGlyph status={wave.status} />
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
          {showProgress && (
            <ProgressBar value={wave.progress} status="running" />
          )}
          {showEta && <span className="when">{wave.eta}</span>}
        </div>
      )}
      {onDelete && (
        <button
          className="wave-row-delete"
          onClick={(e) => {
            e.stopPropagation();
            onDelete();
          }}
          title={`Delete "${wave.title}"`}
          aria-label={`Delete "${wave.title}"`}
        >
          ×
        </button>
      )}
    </div>
  );
}
