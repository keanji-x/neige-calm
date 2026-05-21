import type { Cove, Wave } from '../../types';
import { CardStatusDot } from './CardStatusDot';
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
        {wave.fsmState ? (
          // Per-card FSM is driving this wave — render the 6-state dot inside
          // the same glyph slot so wave row spacing stays identical.
          <span className="glyph">
            <CardStatusDot state={wave.fsmState} />
          </span>
        ) : (
          <WaveGlyph status={wave.status} />
        )}
        <div className="body">
          <div className="t">
            {wave.title}
            {/* Working-card count badge: only when more than one card is
                actively working, since "Working (1)" reads as noise. */}
            {wave.counts && wave.counts.working > 1 && (
              <span
                className="num"
                style={{ marginLeft: 6, opacity: 0.65, fontSize: '0.85em' }}
                title={`${wave.counts.working} cards working`}
              >
                ({wave.counts.working})
              </span>
            )}
          </div>
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
          ×
        </button>
      )}
    </div>
  );
}
