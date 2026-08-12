/**
 * Build-time constants supplied by `vite.config.ts`'s `define`.
 *
 * §0.5 — the version and the build hash are facts about the artifact, not API
 * data: `core/api/generated/wire.ts` has no such fields and the kernel does not
 * serve them. Settings' ABOUT section reads these two, and nothing else.
 */
declare const __NC_VERSION__: string;
declare const __NC_BUILD__: string;
