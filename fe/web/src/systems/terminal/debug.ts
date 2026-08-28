// Call sites were copied with XtermView. fe forbids localStorage in systems,
// so this is intentionally silent rather than gating on `calm.debug`.
export function dlog(..._unused: unknown[]): void {
  void _unused;
}
