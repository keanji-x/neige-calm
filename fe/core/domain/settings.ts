// Workspace settings: a flat string map the kernel stores verbatim.

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';
import type { FailureTable, WriteClass, WriteText } from './failure-class.js';

export const settingsBagSchema = z.object({ settings: z.record(z.string(), z.string()) });
export type SettingsBag = z.infer<typeof settingsBagSchema>;

/** `null` clears a key. Send it explicitly rather than relying on the kernel also treating `""` as a delete. */
export type SettingsPatch = Readonly<Record<string, string | null>>;

export function settingsOperation(): ApiOperation<SettingsBag> {
  return { method: 'GET', path: '/api/settings', responseSchema: settingsBagSchema };
}

export function putSettingsOperation(settings: SettingsPatch): ApiOperation<SettingsBag> {
  return { method: 'PUT', path: '/api/settings', body: { settings }, responseSchema: settingsBagSchema };
}

/**
 * `PUT /api/settings`, a per-key merge. A 400 rejects an invalid Planner duration before any key
 * is stored; other answered 4xx also refuse the patch. Read through `writeFailureText` at the row.
 */
export const SETTINGS_FAILURES: FailureTable<WriteClass> = Object.freeze({
  rules: Object.freeze([Object.freeze({ status: Object.freeze({ from: 400, to: 499 }), is: 'refused' as const })]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

export const SETTINGS_TEXT: WriteText = Object.freeze({ refused: 'It was not saved.', unknown: 'The save is unconfirmed.' });

export const HTTP_PROXY_KEY = 'http_proxy';
export const HTTPS_PROXY_KEY = 'https_proxy';

export const PLANNER_TURN_DURATION_KEY = 'planner_max_turn_duration_minutes';
export const DEFAULT_PLANNER_TURN_DURATION_MINUTES = 30;
export const PLANNER_TURN_DURATION_ERROR = 'Enter a whole number from 1 to 240 minutes (ASCII digits).';

/** Missing means the declared default; corrupt storage is an error, never a default. */
export function plannerTurnDuration(value: string | undefined):
  | Readonly<{ ok: true; minutes: number; isDefault: boolean }>
  | Readonly<{ ok: false; error: string }> {
  if (value === undefined) return { ok: true, minutes: DEFAULT_PLANNER_TURN_DURATION_MINUTES, isDefault: true };
  const minutes = Number(value);
  if (!/^[0-9]+$/.test(value) || !Number.isInteger(minutes) || minutes < 1 || minutes > 240) {
    return { ok: false, error: PLANNER_TURN_DURATION_ERROR };
  }
  return { ok: true, minutes, isDefault: false };
}

/** One-key patch; null explicitly resets even corrupt storage. */
export function plannerTurnDurationPatch(value: string | null): SettingsPatch {
  if (value === null) return { [PLANNER_TURN_DURATION_KEY]: null };
  const parsed = plannerTurnDuration(value);
  if (!parsed.ok) throw new Error(parsed.error);
  return { [PLANNER_TURN_DURATION_KEY]: String(parsed.minutes) };
}
