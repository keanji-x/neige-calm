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
 * `PUT /api/settings`, a per-key merge that is safe to repeat. The handler itself answers only 500; an answered 4xx
 * is the extractor's refusal before anything was stored. Read through `writeFailureText` at the row.
 */
export const SETTINGS_FAILURES: FailureTable<WriteClass> = Object.freeze({
  rules: Object.freeze([Object.freeze({ status: Object.freeze({ from: 400, to: 499 }), is: 'refused' as const })]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

export const SETTINGS_TEXT: WriteText = Object.freeze({ refused: 'It was not saved.', unknown: 'The save is unconfirmed.' });

export const HTTP_PROXY_KEY = 'http_proxy';
export const HTTPS_PROXY_KEY = 'https_proxy';
