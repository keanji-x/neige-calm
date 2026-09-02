// Installed plugins: the list Settings › Plugins reads, and the two lifecycle
// writes it offers.
//
// The list row is deliberately the *compact* one the kernel serves from
// `GET /api/plugins` (`routes/plugins.rs::PluginListItem`) — id, version,
// enabled, runtime state, display name, description, last error. The manifest
// blob is not read here: nothing on this screen renders it, and asking for it
// would make a list of ten plugins ship ten manifests.

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';

/**
 * The kernel's wire-name set for a plugin's runtime state, plus the fallback
 * arm.
 *
 * `unavailable` is the *normal* terminal state of a connector whose bring-up
 * failed — unreachable upstream, rejected secrets, boot budget exhausted — and
 * unlike `crashed` nothing will retry it. The UI must therefore not paint it as
 * a kernel error; `last_error` carries the reason and the row stays a row.
 *
 * The union is `catch`-guarded rather than exhaustive: a kernel that grows an
 * eighth state must not blank the whole Plugins screen with a decode failure,
 * so an unknown wire name degrades to `unknown` and renders as a neutral chip.
 */
export const PLUGIN_STATES = Object.freeze([
  'running', 'spawning', 'crashed', 'unavailable', 'disabled', 'installing', 'installed',
] as const);
export type PluginState = (typeof PLUGIN_STATES)[number] | 'unknown';

const pluginStateSchema: z.ZodType<PluginState> = z
  .enum(PLUGIN_STATES)
  .or(z.string().transform(() => 'unknown' as const));

export const pluginListItemSchema = z.object({
  id: z.string(),
  version: z.string(),
  enabled: z.boolean(),
  state: pluginStateSchema,
  manifest_name: z.string(),
  manifest_description: z.string().optional(),
  last_error: z.string().optional(),
});
export type PluginListItem = z.infer<typeof pluginListItemSchema>;

export function pluginsOperation(): ApiOperation<PluginListItem[]> {
  return { method: 'GET', path: '/api/plugins', responseSchema: z.array(pluginListItemSchema) };
}

/**
 * Enable / disable, as one operation taking the target state.
 *
 * Two endpoints, one intent: the caller says what the plugin should *be*, not
 * which URL to hit. The response is the full detail object; only the fields the
 * list already knows are decoded, because the write's job here is to tell the
 * list what changed, and every other field would be a second, weaker copy of a
 * shape `GET /api/plugins/{id}` owns.
 */
export function setPluginEnabledOperation(id: string, enabled: boolean): ApiOperation<{ id: string; enabled: boolean }> {
  return {
    method: 'POST',
    path: `/api/plugins/${encodeURIComponent(id)}/${enabled ? 'enable' : 'disable'}`,
    responseSchema: z.object({ id: z.string(), enabled: z.boolean() }).loose(),
  };
}
