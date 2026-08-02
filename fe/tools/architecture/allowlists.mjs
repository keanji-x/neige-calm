/**
 * Architecture-rule exceptions. Entries must be repository-relative files,
 * never globs. Keep each reason beside its path and delete stale entries.
 *
 * The new tree currently needs no module-state or React-context exceptions.
 * App providers and UI primitives may be added here only when the owning file
 * exists and the exception has an architecture reason.
 */
/** @type {ReadonlyArray<string>} */
export const moduleRuntimeStateAllowlist = [
  // App bootstrap must retain the browser mount node while composing React.
  'web/src/main.tsx',
];

/** @type {ReadonlyArray<string>} */
export const createContextAllowlist = [
  // The application composition root may own cross-domain provider contexts.
  'web/src/main.tsx',
];
