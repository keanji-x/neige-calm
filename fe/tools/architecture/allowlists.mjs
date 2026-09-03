/**
 * Architecture-rule exceptions. Entries must be repository-relative files,
 * never globs. Keep each reason beside its path and delete stale entries.
 *
 * The new tree currently needs no module-state or React-context exceptions.
 * App providers and UI primitives may be added here only when the owning file
 * exists and the exception has an architecture reason.
 */
export const moduleRuntimeStateExceptions = Object.freeze([
  Object.freeze({ path: 'web/src/main.tsx', reason: 'App bootstrap must retain the browser mount node while composing React.' }),
  Object.freeze({ path: 'web/src/app/router/idempotency-key.ts', reason: 'The fallback mint counter must outlive every panel mount: the conversation panel is a per-page hook, so a remount-scoped counter could reuse a Track conversation key.' }),
  Object.freeze({ path: 'web/src/systems/terminal/xterm-view.tsx', reason: 'Port of web/src/XtermView.tsx: palette tables, protocol constants, and the xterm.js Terminal constructor live at module scope in the original file.' }),
  Object.freeze({ path: 'web/src/systems/terminal/theme-rgb.ts', reason: 'Port of web/src/api/themeRgb.ts; fg/bg tuples must stay mutable [number, number, number] for the terminal wire type.' }),
  Object.freeze({ path: 'web/src/systems/terminal/osc52.ts', reason: 'Port of web/src/input/osc52.ts; encoded-length constant is derived at module scope in the original file.' }),
]);
export const moduleRuntimeStateAllowlist = Object.freeze(moduleRuntimeStateExceptions.map(({ path }) => path));

export const createContextExceptions = Object.freeze([
  Object.freeze({ path: 'web/src/app/conversations/public.tsx', reason: 'The visit-scoped conversation registry provider must sit above the route outlet so navigation cannot discard remembered conversations.' }),
  Object.freeze({ path: 'web/src/app/shell/public.tsx', reason: 'The shell owns mobile workspace-sheet state while Track routes inside <Outlet /> need to reopen the Areas or Pages sheet on Back.' }),
  Object.freeze({ path: 'web/src/app/theme/public.tsx', reason: 'App theme provider owns the document dataset mirror.' }),
  Object.freeze({ path: 'web/src/ui/dialog/public.tsx', reason: 'Issue #997 permits context in the primitive directory while consumers remain in ui.' }),
]);
/** @param {string} name @param {ReadonlyArray<{reason: string}>} entries */
function requireReasons(name, entries) {
  if (entries.some(({ reason }) => reason.trim() === '')) throw new Error(`${name} exceptions require a nonempty reason`);
}
requireReasons('module runtime state', moduleRuntimeStateExceptions);
requireReasons('createContext', createContextExceptions);
export const createContextAllowlist = Object.freeze(createContextExceptions.map(({ path }) => path));
