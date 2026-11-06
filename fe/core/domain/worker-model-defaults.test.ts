import { describe, expect, it } from 'vitest';
import { readWorkerModelDefaults, workerModelDefaultsDraft, workerModelDefaultsPatch } from './worker-model-defaults.js';

describe('Worker defaults codec', () => {
  it('distinguishes absent inheritance from an explicit nullable pair', () => {
    expect(readWorkerModelDefaults(undefined)).toEqual({ ok: true, value: null });
    expect(readWorkerModelDefaults('{"model":null,"effort":null}')).toEqual({ ok: true, value: { model: null, effort: null } });
  });
  it.each([
    '', 'null', '[]', '{', '{}', '{"model":"custom"}', '{"effort":null}',
    '{"model":false,"effort":null}', '{"model":null,"effort":3}',
    '{"model":null,"effort":null,"extra":true}',
    '{"model":"","effort":null}', '{"model":" padded","effort":null}',
    '{"model":null,"effort":"padded "}', '{"model":"line\\nfeed","effort":null}',
    '{"model":"x\\u007fy","effort":null}', '{"model":null,"effort":"x\\u0085y"}',
    '{"model":"one","model":"two","effort":null}',
    '{"model":null,"effort":null,"eff\\u006frt":"other"}',
  ])('keeps corrupt storage explicit: %s', raw => {
    expect(readWorkerModelDefaults(raw).ok).toBe(false);
  });
  it('accepts escaped keys, arbitrary slugs and punctuation without catalog policy', () => {
    const value = { model: 'private/model:"x",effort:y', effort: 'future-effort' };
    expect(readWorkerModelDefaults(JSON.stringify(value))).toEqual({ ok: true, value });
    expect(readWorkerModelDefaults('{"mo\\u0064el":"custom","effort":null}')).toEqual({ ok: true, value: { model: 'custom', effort: null } });
  });
  it('uses null for empty drafts but refuses whitespace and controls', () => {
    expect(workerModelDefaultsDraft('', '')).toEqual({ ok: true, value: { model: null, effort: null } });
    expect(workerModelDefaultsDraft('custom', '')).toEqual({ ok: true, value: { model: 'custom', effort: null } });
    expect(workerModelDefaultsDraft(' custom ', '').ok).toBe(false);
    expect(workerModelDefaultsDraft('', 'x\ny').ok).toBe(false);
  });
  it.each(['codex', 'claude'] as const)('writes and clears exactly the %s pair key', provider => {
    expect(workerModelDefaultsPatch(provider, { model: 'custom', effort: null }))
      .toEqual({ [`worker_model_defaults_${provider}`]: '{"model":"custom","effort":null}' });
    expect(workerModelDefaultsPatch(provider, null)).toEqual({ [`worker_model_defaults_${provider}`]: null });
    expect(() => workerModelDefaultsPatch(provider, { model: ' bad', effort: null })).toThrow();
  });
});
