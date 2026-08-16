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
  // App theme is a cross-route concern whose single provider intentionally owns the document dataset mirror.
  'web/src/app/theme/public.tsx',
  // Reason: issue #997 §4/§6 allow context in a primitive's own directory; consumers remain in ui, so no primitive-to-business reverse dependency is introduced.
  'web/src/ui/dialog/public.tsx',
  // Reason: the shell owns the New wave dialog because the rail and the cove page both open it; the cove route renders inside <Outlet /> and there is no prop path from the shell to it. The context carries one callback and is provided by the shell alone.
  'web/src/app/shell/public.tsx',
];
