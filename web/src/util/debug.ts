// Diagnostic helper. No-op by default. Enable in DevTools console:
//   localStorage.setItem('calm.debug', '1') ; location.reload()
// Disable:
//   localStorage.removeItem('calm.debug') ; location.reload()
//
// Sprinkled `dlog(scope, ...)` callsites in eventBridge, router, WaveGrid,
// TerminalCard, and XtermView trace the multi-step card-create flow + RGL
// layout activity. They were instrumental in finding the original twitch
// (PR #12) and the WaveGrid dual-state feedback loop. Kept gated rather
// than removed so the next person debugging a similar issue doesn't have
// to re-instrument from scratch.

const ENABLED =
  typeof localStorage !== 'undefined' && localStorage.getItem('calm.debug') === '1';

const T0 = typeof performance !== 'undefined' ? performance.now() : 0;

export function dlog(scope: string, ...args: unknown[]): void {
  if (!ENABLED) return;
  const ms = ((typeof performance !== 'undefined' ? performance.now() : 0) - T0).toFixed(1);
  // eslint-disable-next-line no-console
  console.log(`[calm:${scope}] +${ms}ms`, ...args);
}
