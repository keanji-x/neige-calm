import type { ExitChange } from '../../XtermView';

/**
 * #306 — small text-only badge surfacing the child's exit code on the
 * card header. Sits in `<CardHead>`'s status slot alongside
 * (`CardStatusDot`, role pills) so a finished terminal stays a calm
 * card, not a takeover overlay.
 *
 * Coloring follows warp's convention (the design we ported in #306):
 *
 *   - `exit_code === 0`            → success (green-ish)
 *   - `exit_code === 130 || 141`   → neutral (gray) — SIGINT / SIGPIPE
 *                                    are user-aborted, not failures
 *   - any other non-zero code      → error (red)
 *   - `signal_killed === true`     → error (red), label "signal"
 *
 * 130 / 141 share styling with success deliberately: a Ctrl-C'd `tail
 * -f` or a piped command whose downstream closed shouldn't read as
 * failure. The label still reflects the numeric code so the user can
 * tell what happened.
 */
export function CardExitBadge({ exit }: { exit: ExitChange }) {
  const { exit_code, signal_killed } = exit;
  const kind = badgeKind(exit_code, signal_killed);
  const label = badgeLabel(exit_code, signal_killed);
  return (
    <span
      // `role="img"` mirrors `CardStatusDot` — a stateful status glyph
      // that conveys meaning beyond its surrounding text needs a non-
      // generic ARIA role for axe's `aria-prohibited-attr` rule, and
      // the badge is semantically image-like (a status indicator). The
      // numeric label is announced via `aria-label` so screen-readers
      // get the full context, not just "exit".
      role="img"
      aria-label={`${label} exit status`}
      title={label}
      className={`card-head-exit-badge card-head-exit-badge-${kind}`}
    >
      {label}
    </span>
  );
}

type BadgeKind = 'success' | 'neutral' | 'error';

function badgeKind(
  exit_code: number | null,
  signal_killed: boolean,
): BadgeKind {
  if (signal_killed) return 'error';
  if (exit_code === null) {
    // No code recorded and no signal — happens on the WS-close backstop
    // path when the daemon's `TerminalExited` JSON frame got dropped on
    // a slow link. Render the badge in the neutral palette so the user
    // sees "the process exited" without misleading severity.
    return 'neutral';
  }
  if (exit_code === 0) return 'success';
  // Warp's convention: SIGINT (130) and SIGPIPE (141) aren't failures;
  // they're user-initiated aborts or downstream-driven closes. Keep
  // the numeric label but use the calm palette.
  if (exit_code === 130 || exit_code === 141) return 'neutral';
  return 'error';
}

function badgeLabel(
  exit_code: number | null,
  signal_killed: boolean,
): string {
  if (signal_killed) return 'signal';
  if (exit_code === null) return 'exit';
  return `exit ${exit_code}`;
}
