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
  Object.freeze({ path: 'web/src/app/router/idempotency-key.ts', reason: 'The fallback mint counter must outlive every panel mount: the conversation panel is a per-page hook, so a remount-scoped counter would re-mint a key the same cove already used.' }),
]);
export const moduleRuntimeStateAllowlist = Object.freeze(moduleRuntimeStateExceptions.map(({ path }) => path));

export const createContextExceptions = Object.freeze([
  Object.freeze({ path: 'web/src/app/conversations/public.tsx', reason: 'The visit-scoped conversation registry provider must sit above the route outlet so navigation cannot discard remembered conversations.' }),
  Object.freeze({ path: 'web/src/app/shell/public.tsx', reason: 'The shell owns the New wave dialog because the rail and the cove page both open it; the cove route renders inside <Outlet /> and there is no prop path from the shell to it. The context carries one callback and is provided by the shell alone.' }),
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
