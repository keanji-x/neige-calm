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
]);
export const moduleRuntimeStateAllowlist = Object.freeze(moduleRuntimeStateExceptions.map(({ path }) => path));

export const createContextExceptions = Object.freeze([
  Object.freeze({ path: 'web/src/app/theme/public.tsx', reason: 'App theme provider owns the document dataset mirror.' }),
  Object.freeze({ path: 'web/src/ui/dialog/public.tsx', reason: 'Issue #997 permits context in the primitive directory while consumers remain in ui.' }),
]);
export const createContextAllowlist = Object.freeze(createContextExceptions.map(({ path }) => path));
