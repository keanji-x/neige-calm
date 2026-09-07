// #1255 S3 — how full this conversation's context is, beside the send button.
//
// ── What the ring is drawn from, and what it is not ───────────────────────
//
// `percent`, and only `percent`. The kernel computes it
// (`crates/calm-server/src/harness/token_usage.rs`) because the division is
// not the obvious one: the prompt-and-tools floor every thread starts with is
// subtracted from BOTH sides, so `used_tokens / context_window` is a different
// — and wrong — number. That module says it in as many words: "neither
// survives being restated in TypeScript." So nothing here divides anything.
//
// The two raw counts still travel, and they are shown, because "24k of 258k"
// is the thing a person actually wants to know. The tooltip says both, and
// says why they do not divide out to the ring's own figure, so a reader who
// checks the arithmetic finds an explanation rather than a bug.
//
// ── When there is no ring ─────────────────────────────────────────────────
//
// `percent === null` means the kernel declined to state one. Two of its three
// reasons are "nothing to measure against" — no window, or a window at or
// below the floor — and those draw nothing at all: a meter is a claim, and we
// have none to make.
//
// The third is the interesting one and it gets its own state: a count that
// overshot the window. Measured at 4 frames in 181_344 — rare, real, and
// deliberately NOT clamped to a full bar upstream, because clamping renders a
// malfunction as a plausible "context is full" and destroys the evidence. It
// is distinguishable here without re-deriving anything the kernel owns: both
// numbers are on the wire, and `used > window` is a comparison, not the
// percentage formula.

import { Tooltip } from '@astryxdesign/core/Tooltip';

import type { PlannerRunTokenUsage } from '../../../../../core/domain/conversation.ts';
import styles from './context-ring.module.css';

/** `24_100` → `24.1k`; `980` → `980`. Counts only, never a ratio. */
export function formatTokens(count: number): string {
  if (count < 1000) return String(count);
  const thousands = count / 1000;
  /* One decimal below 100k, none above: `258k` is the window every reader has
     seen written that way, and `258.4k` spends a character on precision that
     changes no decision. */
  return thousands < 100
    ? `${(Math.round(thousands * 10) / 10).toString()}k`
    : `${Math.round(thousands).toString()}k`;
}

/**
 * What this reading supports drawing. Three outcomes, and they are three
 * because the middle one is a state and not an absence.
 */
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
  /* The kernel withheld a percentage. Only one of its reasons is something a
     reader can act on, and this is the comparison that finds it. */
  if (usage.context_window !== null && usage.used_tokens > usage.context_window) {
    return { kind: 'over', used: usage.used_tokens, window: usage.context_window };
  }
  return { kind: 'none' };
}

/* 18 and not 16: it stands beside a 32px send button, and at icon size the
   ring read as a speck rather than as a meter. Measured on the bench. */
const SIZE = 18;
const STROKE = 2.5;
const RADIUS = (SIZE - STROKE) / 2;
const CIRCUMFERENCE = 2 * Math.PI * RADIUS;

/**
 * The meter itself.
 *
 * A ring rather than a bar because it has to live in the send button's row at
 * the size of an icon, where a bar long enough to read would be the widest
 * thing in the footer. `stroke-dasharray` on one circle, no library.
 *
 * It is `aria-hidden` and the accessible text lives on the tooltip's trigger:
 * a percentage read out on its own ("thirty-one percent") names no quantity,
 * and the sentence the tooltip already carries is the one worth hearing.
 */
export function ContextRing({ usage }: { usage: PlannerRunTokenUsage | null }) {
  const state = contextRingState(usage);
  if (state.kind === 'none') return null;

  const over = state.kind === 'over';
  const percent = over ? 100 : Math.min(100, Math.max(0, state.percent));
  const label = ringLabel(state);

  return (
    <Tooltip content={<ContextTooltip state={state} />} placement="above">
      <span
        className={`${styles.ring} ${over ? styles.over : ''}`}
        data-nc-context-ring={over ? 'over' : String(Math.round(percent))}
        /*
         * The whole sentence, not a percentage.
         *
         * There is no `tabIndex` here and that is deliberate: this is a
         * readout, not a control, and a tab stop on something that does
         * nothing when pressed is the dead-control shape this composer's own
         * notes reject twice over. What that would have bought is reaching the
         * tooltip by keyboard — so instead the label says everything the
         * tooltip says, and a screen reader gets it without one. The tooltip
         * is then what it should be: the same fact, for a mouse.
         */
        role="img"
        aria-label={label}
      >
        <svg width={SIZE} height={SIZE} viewBox={`0 0 ${SIZE} ${SIZE}`} aria-hidden="true" focusable="false">
          <circle
            className={styles.track}
            cx={SIZE / 2} cy={SIZE / 2} r={RADIUS}
            fill="none" strokeWidth={STROKE}
          />
          <circle
            className={styles.fill}
            cx={SIZE / 2} cy={SIZE / 2} r={RADIUS}
            fill="none" strokeWidth={STROKE} strokeLinecap="round"
            strokeDasharray={`${(CIRCUMFERENCE * percent) / 100} ${CIRCUMFERENCE}`}
            /* Twelve o'clock, clockwise — the direction a dial is read. */
            transform={`rotate(-90 ${SIZE / 2} ${SIZE / 2})`}
          />
        </svg>
      </span>
    </Tooltip>
  );
}

/**
 * The two lines inside the tooltip.
 *
 * Plain spans that inherit their colour, and NOT `Text`. Astryx's tooltip is
 * an inverted surface — dark ground, light type, its own note says so — and
 * `Text` re-asserts `--color-text-primary`, which in this app is the near-black
 * used on paper. Measured on the bench: the first line rendered black on black
 * and simply was not there. The design system's answer for type on somebody
 * else's surface is to inherit, so these inherit.
 */
/** The readout as one sentence, for anything that cannot hover. */
function ringLabel(state: ContextRingState): string {
  if (state.kind === 'over') {
    return `${formatTokens(state.used)} of a ${formatTokens(state.window)} context window `
      + '— more than the window holds, so no percentage is shown';
  }
  if (state.kind === 'filled') {
    const counts = state.window === null
      ? `${formatTokens(state.used)} in context`
      : `${formatTokens(state.used)} of ${formatTokens(state.window)} in context`;
    return `${counts}, ${Math.round(state.percent)}% of what this thread can use`;
  }
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
        {/* Said plainly rather than drawn as a full ring: the count is larger
            than the window it is measured against, which is not a context
            that is full — it is a reading that cannot be one. */}
        <span className={styles.tipNote}>
          That is more than the window holds, so no percentage is shown.
        </span>
      </span>
    );
  }
  return (
    <span className={styles.tip}>
      <span className={styles.tipLead}>
        {formatTokens(state.used)}
        {state.window === null ? ' in context' : ` of ${formatTokens(state.window)} in context`}
      </span>
      {/* Why the ring is not those two numbers divided. Stated without naming
          the floor's size, which is the kernel's constant and not ours. */}
      <span className={styles.tipNote}>
        {Math.round(state.percent)}% of what this thread can use — the prompt and
        tools it starts with do not count toward the ring.
      </span>
    </span>
  );
}
