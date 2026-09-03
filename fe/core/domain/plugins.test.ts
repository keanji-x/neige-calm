import { describe, expect, it } from 'vitest';

import {
  configDraftFrom, configFieldsOf, configPatchFrom, configWriteError, patchPluginConfigOperation,
  pluginDetailSchema, pluginListItemSchema, reloadOutcome, reloadPluginOperation, storedConfigOf,
} from './plugins.js';

/**
 * A `config_schema` in the shape the kernel publishes it.
 *
 * Written as the kernel's subset states it (`plugin_host::template_input`'s
 * module doc): root `type: "object"` with an explicit
 * `additionalProperties: false`, and per property a type from
 * `{string, integer, number, boolean}` plus optional `enum` / `default` /
 * `description`. `enum` appears only on a string, because that is the only
 * place the kernel accepts one.
 */
function schema(): unknown {
  return {
    type: 'object',
    additionalProperties: false,
    required: ['token'],
    properties: {
      token: { type: 'string', description: 'API token for the forge.' },
      base_url: { type: 'string', default: 'https://api.github.com' },
      mode: { type: 'string', enum: ['read', 'write'], default: 'read' },
      verbose: { type: 'boolean', default: true },
      retries: { type: 'integer', default: 3 },
      timeout: { type: 'number' },
    },
  };
}

describe('plugin list rows', () => {
  it('requires has_config rather than inventing it', () => {
    const row = {
      id: 'git-forge', version: '0.1.0', enabled: true, state: 'running', manifest_name: 'Git forge',
    };
    // A kernel that did not send the bit is not the kernel this screen was
    // written against, and "no configurable keys" must never be a guess.
    expect(pluginListItemSchema.safeParse(row).success).toBe(false);
    expect(pluginListItemSchema.safeParse({ ...row, has_config: false }).success).toBe(true);
  });
});

describe('plugin detail', () => {
  it('decodes a user_config the kernel refuses to merge into, instead of failing', () => {
    /* #1284's 409 `plugin_config_corrupt` row: the kernel keeps a non-object
       `user_config` rather than coercing it away, and publishes it verbatim. A
       schema that demanded an object here would turn the one row that needs
       repairing into a blank screen. */
    const decoded = pluginDetailSchema.parse({
      id: 'git-forge',
      version: '0.1.0',
      enabled: true,
      state: 'running',
      manifest: { id: 'git-forge' },
      config_schema: schema(),
      user_config: 'not an object',
      effective_config: {},
      installed_at: 0,
      updated_at: 0,
    });
    expect(storedConfigOf(decoded.user_config)).toBeNull();
    expect(storedConfigOf({ token: 't' })).toEqual({ token: 't' });
  });

  it('degrades an unknown runtime state instead of blanking the screen', () => {
    const decoded = pluginDetailSchema.parse({
      id: 'x', version: '1', enabled: true, state: 'hibernating', user_config: {}, effective_config: {},
    });
    expect(decoded.state).toBe('unknown');
  });
});

describe('configFieldsOf', () => {
  it('reads the kernel subset, in declaration order', () => {
    const fields = configFieldsOf(schema());
    expect(fields.map((field) => field.key)).toEqual([
      'token', 'base_url', 'mode', 'verbose', 'retries', 'timeout',
    ]);
    expect(fields.map((field) => field.kind)).toEqual([
      'string', 'string', 'string', 'boolean', 'integer', 'number',
    ]);
    expect(fields[0]?.required).toBe(true);
    expect(fields[1]?.required).toBe(false);
    expect(fields[0]?.description).toBe('API token for the forge.');
    expect(fields[2]?.options).toEqual(['read', 'write']);
    expect(fields[3]?.default).toBe(true);
  });

  it('drops a property whose type is outside the subset rather than guessing a control', () => {
    // A control the kernel would refuse a value for is worse than no control:
    // the operator fills it in and the write 400s on a key they cannot fix.
    const fields = configFieldsOf({
      type: 'object',
      additionalProperties: false,
      properties: {
        good: { type: 'string' },
        nested: { type: 'object', properties: {} },
        list: { type: 'array' },
      },
    });
    expect(fields.map((field) => field.key)).toEqual(['good']);
  });

  it('answers with nothing for anything that is not a schema at all', () => {
    for (const value of [undefined, null, 'string', 42, [], {}, { type: 'object' }]) {
      expect(configFieldsOf(value)).toEqual([]);
    }
  });
});

describe('the draft a form starts from', () => {
  it('seeds from what the operator set, never from the manifest defaults', () => {
    const fields = configFieldsOf(schema());
    const draft = configDraftFrom(fields, { token: 'abc' });
    expect(draft.token).toBe('abc');
    /* `base_url` has a default and no stored value: it starts **empty**, so the
       default can show as a placeholder and stay out of every payload. A draft
       seeded from `effective_config` would start at the default and post it
       back on the first Save. */
    expect(draft.base_url).toBeNull();
    expect(draft.mode).toBeNull();
    expect(draft.retries).toBeNull();
    /* The one control with no third position and no placeholder: a switch shows
       the default it would run with. The diff base is this same state, so
       showing it costs no write. */
    expect(draft.verbose).toBe(true);
  });

  it('starts a stored value of the wrong type empty rather than mangling it', () => {
    const fields = configFieldsOf(schema());
    const draft = configDraftFrom(fields, { retries: 'three' });
    expect(draft.retries).toBeNull();
    // And leaves it alone: the patch says nothing about a field nobody edited.
    expect(configPatchFrom(fields, draft, draft)).toEqual({});
  });

  it('treats a corrupt stored document as no values at all', () => {
    const fields = configFieldsOf(schema());
    expect(configDraftFrom(fields, null)).toEqual({
      token: null, base_url: null, mode: null, verbose: true, retries: null, timeout: null,
    });
  });
});

describe('configPatchFrom (#1284 §2.2.5)', () => {
  const fields = configFieldsOf(schema());

  it('carries only the keys that were edited', () => {
    const base = configDraftFrom(fields, { token: 'abc', retries: 5 });
    const patch = configPatchFrom(fields, base, { ...base, token: 'xyz' });
    expect(patch).toEqual({ token: 'xyz' });
  });

  it('never writes a manifest default back', () => {
    /*
     * The load-bearing case. `base_url`, `mode`, `retries` and `verbose` all
     * have defaults and none is stored; the operator edits `token` only. If the
     * form posted its effective state, this Save would materialize four
     * defaults into the row — and a manifest that later changed any of them
     * would never reach this plugin again.
     */
    const base = configDraftFrom(fields, {});
    const patch = configPatchFrom(fields, base, { ...base, token: 'abc' });
    expect(patch).toEqual({ token: 'abc' });
    expect(Object.keys(patch)).not.toContain('base_url');
    expect(Object.keys(patch)).not.toContain('verbose');
  });

  it('sends null for a value the operator cleared, and nothing for one that was never set', () => {
    const base = configDraftFrom(fields, { token: 'abc' });
    const patch = configPatchFrom(fields, base, { ...base, token: null, base_url: null });
    expect(patch).toEqual({ token: null });
  });

  it('says nothing about a switch flipped back to where it started', () => {
    const base = configDraftFrom(fields, {});
    expect(configPatchFrom(fields, base, { ...base, verbose: false })).toEqual({ verbose: false });
    expect(configPatchFrom(fields, base, { ...base, verbose: true })).toEqual({});
  });

  it('cannot touch a key the current schema does not declare', () => {
    /* Residue from an older manifest is deliberately kept by the kernel and
       shown by nothing. A form that emitted it would delete or rewrite values
       for keys the operator cannot even see. */
    const base = configDraftFrom(fields, { token: 'abc', legacy_flag: true });
    const patch = configPatchFrom(fields, { ...base, legacy_flag: true }, { ...base, legacy_flag: false });
    expect(patch).toEqual({});
  });
});

describe('the operations', () => {
  it('names ?reset=true in the URL only when it is asked for', () => {
    expect(patchPluginConfigOperation('git-forge', { token: 'a' }).path)
      .toBe('/api/plugins/git-forge/config');
    expect(patchPluginConfigOperation('git-forge', { token: 'a' }, { reset: true }).path)
      .toBe('/api/plugins/git-forge/config?reset=true');
  });

  it('sends the patch as the body, and PATCH as the method', () => {
    const operation = patchPluginConfigOperation('git-forge', { token: null });
    expect(operation.method).toBe('PATCH');
    expect(operation.body).toEqual({ token: null });
  });

  it('escapes an id that would otherwise reshape the path', () => {
    expect(reloadPluginOperation('a/b').path).toBe('/api/plugins/a%2Fb/reload');
  });
});

describe('configWriteError', () => {
  const fields = configFieldsOf(schema());

  it('puts a schema violation on the field the kernel named', () => {
    // The kernel's own wording, rooted at `config` because the route passes
    // that as the validator's root path.
    const error = configWriteError(
      { code: 'bad_request', message: 'config.retries: expected integer, found a string' },
      fields,
    );
    expect(error.fieldKey).toBe('retries');
    expect(error.message).toBe('expected integer, found a string');
  });

  it('keeps a violation of an undeclared key off the form', () => {
    /* Same message shape, no control to land on: the form does not render
       `ghost`, so attributing it to a field would mean attaching an error to
       nothing. It belongs on the pane. */
    const error = configWriteError(
      { code: 'bad_request', message: 'config.ghost: unknown field (schema declares additionalProperties: false)' },
      fields,
    );
    expect(error.fieldKey).toBeNull();
    expect(error.message).toContain('unknown field');
  });

  it('says nothing was saved when the lock was held', () => {
    const error = configWriteError({ code: 'plugin_busy', message: 'plugin `git-forge` is busy' }, fields);
    expect(error.fieldKey).toBeNull();
    expect(error.offersReset).toBe(false);
    expect(error.message).toMatch(/nothing was saved/);
    expect(error.message).toMatch(/try again/i);
  });

  it('offers the reset only for the refusal whose exit it is', () => {
    const corrupt = configWriteError(
      { code: 'plugin_config_corrupt', message: 'stored user_config is not a JSON object' },
      fields,
    );
    expect(corrupt.offersReset).toBe(true);
    // The kernel's sentence, not a paraphrase: it names what is wrong.
    expect(corrupt.message).toContain('not a JSON object');

    const unloaded = configWriteError(
      { code: 'plugin_manifest_unloaded', message: 'manifest is not loaded in the kernel registry; reload the plugin' },
      fields,
    );
    expect(unloaded.offersReset).toBe(false);
    expect(unloaded.message).toContain('reload the plugin');
  });
});

describe('reloadOutcome (#1284 §2.4)', () => {
  it('reports a held lock as saved-but-not-restarted', () => {
    /* Row 1: the configuration is in the database, the old process is still up
       on the old configuration, and retrying is the whole remedy. Anything that
       read only the status code would report this as a failed save. */
    const outcome = reloadOutcome({
      failure: { code: 'plugin_busy', message: 'plugin `git-forge` is busy' },
      state: 'running',
    });
    expect(outcome.kind).toBe('busy');
    expect(outcome.tone).toBe('warning');
    expect(outcome.message).toMatch(/saved/i);
    expect(outcome.message).toMatch(/previous configuration/);
  });

  it('carries last_error verbatim when the plugin landed in unavailable', () => {
    /* Row 2: a connector's bring-up failed. `unavailable` is its normal
       terminal state — not a kernel error — and `last_error` is the only
       diagnostic that exists, so it is reproduced word for word. */
    const reason = 'mcp-http: connect to https://api.example.com failed: connection refused';
    const outcome = reloadOutcome({
      failure: { code: 'bad_request', message: 'reload failed' },
      state: 'unavailable',
      lastError: reason,
    });
    expect(outcome.kind).toBe('unavailable');
    expect(outcome.message).toContain(reason);
  });

  it('reads unavailable off the state even when the reload answered 200', () => {
    // The status code is not the verdict, in both directions.
    const outcome = reloadOutcome({ failure: null, state: 'unavailable', lastError: 'upstream said no' });
    expect(outcome.kind).toBe('unavailable');
    expect(outcome.message).toContain('upstream said no');
  });

  it('says an app that did not come back has stopped', () => {
    /* Row 3: a reload stops the plugin before re-reading anything, so this is
       never "carried on with the old configuration". */
    const outcome = reloadOutcome({
      failure: { code: 'bad_request', message: 'spawn failed: No such file or directory' },
      state: 'installed',
    });
    expect(outcome.kind).toBe('stopped');
    expect(outcome.message).toMatch(/stopped/);
    expect(outcome.message).toContain('spawn failed: No such file or directory');
  });

  it('confirms only when something is actually running the new configuration', () => {
    expect(reloadOutcome({ failure: null, state: 'running' })).toMatchObject({
      kind: 'applied', tone: 'success',
    });
    expect(reloadOutcome({ failure: null, state: 'spawning' })).toMatchObject({
      kind: 'starting', tone: 'success',
    });
    /* Saved, reloaded, and nothing holds it: a disabled plugin re-reads its
       manifest and stays put. Calling that success would tell the operator
       their configuration is in force when no process has it. */
    const idle = reloadOutcome({ failure: null, state: 'disabled' });
    expect(idle.kind).toBe('idle');
    expect(idle.tone).toBe('warning');
    expect(idle.message).toMatch(/enable/i);
  });

  it('falls back to the refusal when the plugin could not be read back', () => {
    const outcome = reloadOutcome({
      failure: { code: 'transport_failure', message: 'The request could not be completed.' },
      state: 'unknown',
    });
    expect(outcome.kind).toBe('stopped');
    expect(outcome.message).toContain('The request could not be completed.');
  });
});
