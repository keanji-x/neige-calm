// How full this conversation's context is, beside the send button. Drawn from the kernel's
// `percent` only: the prompt-and-tools floor is taken off both sides, so `used / window` is a different, wrong number.

import { Tooltip } from '@astryxdesign/core/Tooltip';

import type { PlannerRunTokenUsage } from '../../../../../core/domain/conversation.ts';
import styles from './context-ring.module.css';

/** `24_100` → `24.1k`; `980` → `980`. Counts only, never a ratio. */
export function formatTokens(count: number): string {
  if (count < 1000) return String(count);
  const thousands = count / 1000;
  return thousands < 100
    ? `${(Math.round(thousands * 10) / 10).toString()}k`
    : `${Math.round(thousands).toString()}k`;
}

/** What this reading supports drawing; the over-window case is a state, not an absence. */
export type ContextRingState =
  | Readonly<{ kind: 'none' }>
  | Readonly<{ kind: 'over'; used: number; window: number }>
  | Readonly<{ kind: 'filled'; percent: number; used: number; window: number | null }>;

export function contextRingState(usage: PlannerRunTokenUsage | null): ContextRingState {
  if (usage === null) return { kind: 'none' };
  if (usage.percent !== null) {
    return {
      kind: 'filled', percent: usage.percent, used: usage.used_tokens,
      window: usage.context_window,
    };
  }
  /* Of the kernel's reasons to withhold a percentage, only the overshoot is one a reader can act on. */
  if (usage.context_window !== null && usage.used_tokens > usage.context_window) {
    return { kind: 'over', used: usage.used_tokens, window: usage.context_window };
  }
  return { kind: 'none' };
}

/* 18 not 16: at icon size beside a 32px send button the ring read as a speck. */
const SIZE = 18;
const STROKE = 2.5;
const RADIUS = (SIZE - STROKE) / 2;
const CIRCUMFERENCE = 2 * Math.PI * RADIUS;

/** The meter. `aria-hidden`; the accessible text lives on the tooltip's trigger. */
export function ContextRing({ usage }: { usage: PlannerRunTokenUsage | null }) {
  const state = contextRingState(usage);
  if (state.kind === 'none') return null;

  const over = state.kind === 'over';
  /* The over-window state draws no arc: the kernel withholds `percent` rather than clamping, and a full ring here would undo that. */
  const percent = over ? 0 : Math.min(100, Math.max(0, state.percent));
  const label = ringLabel(state);

  return (
    <Tooltip content={<ContextTooltip state={state} />} placement="above">
      <span
        className={`${styles.ring} ${over ? styles.over : ''}`}
        data-nc-context-ring={over ? 'over' : String(Math.round(percent))}
        /* No `tabIndex`: a readout, not a control. The label says everything the tooltip says instead. */
        role="img"
        aria-label={label}
      >
        <svg width={SIZE} height={SIZE} viewBox={`0 0 ${SIZE} ${SIZE}`} aria-hidden="true" focusable="false">
          <circle
            className={styles.track}
            cx={SIZE / 2} cy={SIZE / 2} r={RADIUS}
            fill="none" strokeWidth={STROKE}
          />
          {/* `strokeLinecap: round` paints a dot at a dash length of zero, so a zero-percent arc must be no element. */}
          {percent > 0 && (
            <circle
              className={styles.fill}
              cx={SIZE / 2} cy={SIZE / 2} r={RADIUS}
              fill="none" strokeWidth={STROKE} strokeLinecap="round"
              strokeDasharray={`${(CIRCUMFERENCE * percent) / 100} ${CIRCUMFERENCE}`}
              /* Twelve o'clock, clockwise — the direction a dial is read. */
              transform={`rotate(-90 ${SIZE / 2} ${SIZE / 2})`}
            />
          )}
        </svg>
      </span>
    </Tooltip>
  );
}

/** Plain spans that inherit colour, not `Text`: Astryx's tooltip is an inverted surface and `Text` re-asserts `--color-text-primary`, which rendered black on black. */
/** The readout as one sentence, for anything that cannot hover. */
function ringLabel(state: ContextRingState): string {
  if (state.kind === 'over') {
    return `${formatTokens(state.used)} of a ${formatTokens(state.window)} context window `
      + '— more than the window holds, so no percentage is shown';
  }
  if (state.kind === 'filled') return contextCounts(state);
  return '';
}

function ContextTooltip({ state }: { state: ContextRingState }) {
  if (state.kind === 'none') return null;
  if (state.kind === 'over') {
    return (
      <span className={styles.tip}>
        <span className={styles.tipLead}>
          {formatTokens(state.used)} of a {formatTokens(state.window)} window
        </span>
        <span className={styles.tipNote}>
          That is more than the window holds, so no percentage is shown.
        </span>
      </span>
    );
  }
  return <span className={styles.tipLead}>{contextCounts(state)}</span>;
}

/** `24.1k of 258k in context`. The one sentence both the tooltip and the
 *  label say, so the two cannot drift apart. */
function contextCounts(state: Extract<ContextRingState, { kind: 'filled' }>): string {
  return state.window === null
    ? `${formatTokens(state.used)} in context`
    : `${formatTokens(state.used)} of ${formatTokens(state.window)} in context`;
}
