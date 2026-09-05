// Workspace settings: a flat string map the kernel stores verbatim.

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';

export const settingsBagSchema = z.object({ settings: z.record(z.string(), z.string()) });
export type SettingsBag = z.infer<typeof settingsBagSchema>;

/**
 * `null` clears a key. The kernel treats an empty string as a delete too, so a
 * form that blanks a field and one that removes it converge on the same state
 * — send `null` explicitly rather than relying on that equivalence.
 */
export type SettingsPatch = Readonly<Record<string, string | null>>;

export function settingsOperation(): ApiOperation<SettingsBag> {
  return { method: 'GET', path: '/api/settings', responseSchema: settingsBagSchema };
}

export function putSettingsOperation(settings: SettingsPatch): ApiOperation<SettingsBag> {
  return { method: 'PUT', path: '/api/settings', body: { settings }, responseSchema: settingsBagSchema };
}

export const HTTP_PROXY_KEY = 'http_proxy';
export const HTTPS_PROXY_KEY = 'https_proxy';
export const TASK_BUDGET_DEFAULT_KEY = 'task_budget_default';
export const FALLBACK_TASK_BUDGET_DEFAULT = 1;

/** The API normally returns the effective value; this fallback also keeps
 * older kernels and test transports safe during a rolling frontend update. */
export function taskBudgetDefaultFrom(settings: Readonly<Record<string, string>>): number {
  const raw = settings[TASK_BUDGET_DEFAULT_KEY]?.trim();
  if (raw === undefined || !/^[1-9]\d*$/.test(raw)) return FALLBACK_TASK_BUDGET_DEFAULT;
  const value = Number(raw);
  return Number.isSafeInteger(value) && value > 0 ? value : FALLBACK_TASK_BUDGET_DEFAULT;
}
