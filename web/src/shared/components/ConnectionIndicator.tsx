// ConnectionIndicator — surfaces WS reconnect state in the title bar.
//
// Behavior contract:
//   * `connected` → renders nothing. Happy path is silent; the UI doesn't
//     need to remind the user that everything's fine.
//   * `disconnected` → renders nothing. This state only fires on explicit
//     `close()` (component unmount) — there's nothing meaningful to show
//     because we're not even trying to reconnect.
//   * `connecting` → renders a small status pill with a pulsing dot and
//     the word "reconnecting". The user-visible state is the same whether
//     it's the very first connection or a backoff retry, so we collapse
//     both into one message.
//
// Accessibility:
//   * `role="status"` + `aria-live="polite"` lets screen readers announce
//     the state change without interrupting other speech. The visible
//     text is the announcement payload — the pulsing dot is purely
//     decorative and intentionally not in the AT tree.
//   * Color contrast against `.bar` background passes WCAG AA in both
//     themes via the `--warn` token (the `[data-theme="dark"]` override
//     keeps it readable). See `web/e2e/a11y-axe.spec.ts` for the gate.

import { useConnectionState } from '../../app/useConnectionState';

export function ConnectionIndicator() {
  const state = useConnectionState();
  if (state !== 'connecting') return null;
  return (
    <span
      className="conn-indicator"
      role="status"
      aria-live="polite"
      data-testid="conn-indicator"
    >
      <span className="conn-indicator-dot" aria-hidden="true" />
      reconnecting
    </span>
  );
}
