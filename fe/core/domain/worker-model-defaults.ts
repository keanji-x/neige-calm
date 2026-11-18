// Worker publication defaults: one strict JSON string and one Settings key per provider.
import { z } from 'zod';
import type { WorkerModelDefaults, WorkerModelProvider } from '../api/generated/wire.ts';
import type { SettingsPatch } from './settings.ts';

export const WORKER_MODEL_PROVIDERS = Object.freeze(['codex', 'claude'] as const satisfies readonly WorkerModelProvider[]);
const KEYS: Readonly<Record<WorkerModelProvider, string>> = Object.freeze({
  codex: 'worker_model_defaults_codex', claude: 'worker_model_defaults_claude',
});
export function workerModelDefaultsKey(provider: WorkerModelProvider): string { return KEYS[provider]; }

const slug = z.string().min(1).refine(value => value.trim() === value && !/\p{Cc}/u.test(value));
const pair = z.strictObject({ model: slug.nullable(), effort: slug.nullable() });
export const WORKER_DEFAULTS_ERROR = 'Model and effort must both be present, each null or a nonempty slug without surrounding whitespace or control characters.';
export type WorkerDefaultsRead = Readonly<{ ok: true; value: WorkerModelDefaults | null } | { ok: false; error: string }>;

/** Missing is inheritance; corrupt (including duplicate members) stays an explicit error. */
export function readWorkerModelDefaults(raw: string | undefined): WorkerDefaultsRead {
  if (raw === undefined) return { ok: true, value: null };
  try {
    const parsed = pair.safeParse(JSON.parse(raw) as unknown);
    // After strict flat-shape validation, tokenize every JSON string (including values)
    // so escaped keys count and colons inside string values cannot look like members.
    const members = [...raw.matchAll(/("(?:\\.|[^"\\])*")\s*(:?)/g)].filter(match => match[2] === ':');
    if (parsed.success && members.length === 2) return { ok: true, value: parsed.data };
  } catch { /* Malformed JSON is the same repairable storage failure. */ }
  return { ok: false, error: WORKER_DEFAULTS_ERROR };
}

/** Empty editor fields mean null; whitespace is rejected, never silently normalized. */
export function workerModelDefaultsDraft(model: string, effort: string): WorkerDefaultsRead {
  const parsed = pair.safeParse({ model: model === '' ? null : model, effort: effort === '' ? null : effort });
  return parsed.success ? { ok: true, value: parsed.data } : { ok: false, error: WORKER_DEFAULTS_ERROR };
}

/** One provider, one atomic pair; outer null deletes that provider's entire key. */
export function workerModelDefaultsPatch(provider: WorkerModelProvider, value: WorkerModelDefaults | null): SettingsPatch {
  if (value === null) return { [KEYS[provider]]: null };
  return { [KEYS[provider]]: JSON.stringify(pair.parse(value)) };
}
