import { describe, expect, it } from 'vitest';

import {
  EMPTY_CONNECTOR_DRAFT, configDraftFrom, configFieldsOf, configPatchFrom, configWriteError,
  connectorDraftError, installConnectorOperation, installLocalPathOperation, toolsAllowOf,
  patchPluginConfigOperation, pluginDetailSchema, pluginListItemSchema, reloadOutcome,
  reloadPluginOperation, storedConfigOf, uninstallPluginOperation,
  type ConnectorInstallDraft,
} from './plugins.js';

/**
 * A `config_schema` in the kernel's subset: root `type: "object"` with `additionalProperties:
 * false`; `enum` only on a string.
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
    expect(pluginListItemSchema.safeParse(row).success).toBe(false);
    expect(pluginListItemSchema.safeParse({ ...row, has_config: false }).success).toBe(true);
  });
});

describe('plugin detail', () => {
  it('decodes a user_config the kernel refuses to merge into, instead of failing', () => {
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
    /* A default with no stored value starts empty, so the default can show as a placeholder and
       stay out of every payload. */
    expect(draft.base_url).toBeNull();
    expect(draft.mode).toBeNull();
    expect(draft.retries).toBeNull();
    expect(draft.verbose).toBe(true);
  });

  it('starts a stored value of the wrong type empty rather than mangling it', () => {
    const fields = configFieldsOf(schema());
    const draft = configDraftFrom(fields, { retries: 'three' });
    expect(draft.retries).toBeNull();
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

  it('deletes the key when a stored boolean is moved back onto its default', () => {
    const base = configDraftFrom(fields, { verbose: false });
    expect(base.verbose).toBe(false);
    expect(configPatchFrom(fields, base, { ...base, verbose: true })).toEqual({ verbose: null });
  });

  it('still writes a boolean moved away from its default', () => {
    const base = configDraftFrom(fields, { verbose: true });
    expect(configPatchFrom(fields, base, { ...base, verbose: false })).toEqual({ verbose: false });
  });

  it('writes a boolean literally when the manifest declares no default for it', () => {
    const undeclared = configFieldsOf({
      type: 'object',
      properties: { flag: { type: 'boolean' } },
    });
    const base = configDraftFrom(undeclared, {});
    expect(base.flag).toBe(false);
    expect(configPatchFrom(undeclared, base, { flag: true })).toEqual({ flag: true });
  });

  it('cannot touch a key the current schema does not declare', () => {
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
    const error = configWriteError(
      { code: 'bad_request', message: 'config.retries: expected integer, found a string' },
      fields,
    );
    expect(error.fieldKey).toBe('retries');
    expect(error.message).toBe('expected integer, found a string');
  });

  it('lands a violation on the declared key it names, not on one that starts the same', () => {
    const pair = configFieldsOf({
      type: 'object',
      properties: { token: { type: 'string' }, token_extra: { type: 'string' } },
    });
    expect(configWriteError(
      { code: 'bad_request', message: 'config.token_extra: expected string, found a number' },
      pair,
    ).fieldKey).toBe('token_extra');
    expect(configWriteError(
      { code: 'bad_request', message: 'config.token: expected string, found a number' },
      pair,
    ).fieldKey).toBe('token');
  });

  it('offers the reset for the byte-cap refusal too, without reading the prose', () => {
    const tooLarge = configWriteError(
      {
        code: 'plugin_config_too_large',
        message: 'config: storing this patch would make plugin `git-forge`\'s user_config 40000 '
          + 'bytes, over the 32768-byte cap. Resend this request with `?reset=true`',
      },
      fields,
    );
    expect(tooLarge.offersReset).toBe(true);
    expect(tooLarge.fieldKey).toBeNull();
    expect(tooLarge.message).toContain('32768');

    expect(configWriteError(
      { code: 'bad_request', message: 'something mentioning ?reset=true in passing' },
      fields,
    ).offersReset).toBe(false);
  });

  it('keeps a violation of an undeclared key off the form', () => {
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
    /* `unavailable` is a connector's normal terminal state, not a kernel error; `last_error` is the only diagnostic. */
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
    const outcome = reloadOutcome({ failure: null, state: 'unavailable', lastError: 'upstream said no' });
    expect(outcome.kind).toBe('unavailable');
    expect(outcome.message).toContain('upstream said no');
  });

  it('says an app that did not come back has stopped', () => {
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
    const idle = reloadOutcome({ failure: null, state: 'disabled' });
    expect(idle.kind).toBe('idle');
    expect(idle.tone).toBe('warning');
    expect(idle.message).toMatch(/enable/i);
  });

  it('says the state is unknown when the request never left the browser', () => {
    const outcome = reloadOutcome({
      failure: { code: 'transport_failure', message: 'The request could not be completed.' },
      state: 'unknown',
    });
    expect(outcome.kind).toBe('unknown');
    expect(outcome.tone).toBe('warning');
    expect(outcome.message).toMatch(/unknown/);
    expect(outcome.message).not.toMatch(/has stopped/);
    expect(outcome.message).toMatch(/saved/i);
  });

  it('still reports a stop when the kernel answered and the plugin is down', () => {
    const outcome = reloadOutcome({
      failure: { code: 'internal', message: 'spawn failed' },
      state: 'unknown',
    });
    expect(outcome.kind).toBe('stopped');
  });

  it('paints unavailable as a warning rather than an error', () => {
    expect(reloadOutcome({ failure: null, state: 'unavailable', lastError: 'upstream said no' }).tone)
      .toBe('warning');
    expect(reloadOutcome({ failure: null, state: 'unavailable' }).tone).toBe('warning');
  });
});

describe('connector install', () => {
  const draft: ConnectorInstallDraft = {
    ...EMPTY_CONNECTOR_DRAFT,
    id: 'com.example.zhibao',
    display_name: 'Zhibao',
    url: 'https://mcp.wisburg.com/mcp',
    api_key: 'sk-credential',
    tool_mode: 'selected',
    tools: 'list-articles, get-article-detail',
  };

  it('defaults a connector with no tool names to the complete upstream catalog', () => {
    const allTools = { ...draft, tool_mode: 'all' as const, tools: '' };
    expect(connectorDraftError(allTools)).toBeNull();
    const source = (installConnectorOperation(allTools).body as {
      source: Record<string, unknown>;
    }).source;
    expect('tools_allow' in source).toBe(false);
  });

  it('sends a bearer credential as the kernel spells it', () => {
    const operation = installConnectorOperation(draft);
    expect(operation.method).toBe('POST');
    expect(operation.path).toBe('/api/plugins/install');
    expect(operation.body).toEqual({
      source: {
        kind: 'mcp_http_v2',
        id: 'com.example.zhibao',
        display_name: 'Zhibao',
        url: 'https://mcp.wisburg.com/mcp',
        tools_allow: ['list-articles', 'get-article-detail'],
        api_key: 'sk-credential',
        api_key_in: 'bearer',
      },
    });
  });

  it('names the header for a server that wants the bare key', () => {
    const body = installConnectorOperation({
      ...draft, placement: 'header', header_name: 'X-API-Key',
    }).body as { source: Record<string, unknown> };
    expect(body.source.api_key_in).toBe('header:X-API-Key');
  });

  /* A blank credential must leave the key out: the kernel reads an absent key as unauthenticated
     and refuses an empty one. */
  it('omits the credential and its placement when none was given', () => {
    const body = installConnectorOperation({ ...draft, api_key: '   ' })
      .body as { source: Record<string, unknown> };
    expect('api_key' in body.source).toBe(false);
    expect('api_key_in' in body.source).toBe(false);
  });

  it('trims what was typed, so a stray space is not part of the id or the URL', () => {
    const body = installConnectorOperation({
      ...draft, id: ' com.example.zhibao ', url: ' https://mcp.wisburg.com/mcp ',
    }).body as { source: Record<string, string> };
    expect(body.source.id).toBe('com.example.zhibao');
    expect(body.source.url).toBe('https://mcp.wisburg.com/mcp');
  });

  /* `tools_allow` is a strict allowlist, so a selected connector with no names would come up
     running and expose nothing. */
  it('refuses a connector that would expose no tools, and splits the list the operator typed', () => {
    expect(connectorDraftError({ ...draft, tools: '   ' })).toMatch(/at least one tool/i);
    expect(toolsAllowOf({ ...draft, tools: 'a, b\nc  d,,a' })).toEqual(['a', 'b', 'c', 'd']);
  });

  it('refuses an empty required field and a placement it cannot spell', () => {
    expect(connectorDraftError({ ...draft, id: '' })).toMatch(/id/i);
    expect(connectorDraftError({ ...draft, display_name: '' })).toMatch(/name/i);
    expect(connectorDraftError({ ...draft, url: '' })).toMatch(/URL/i);
    expect(connectorDraftError({ ...draft, placement: 'header', header_name: '' }))
      .toMatch(/header name/i);
    expect(connectorDraftError(draft)).toBeNull();
    expect(connectorDraftError({ ...draft, api_key: '', placement: 'header', header_name: '' }))
      .toBeNull();
  });

  it('addresses uninstall at the plugin, with its id escaped', () => {
    const operation = uninstallPluginOperation('com.example/zhibao');
    expect(operation.method).toBe('DELETE');
    expect(operation.path).toBe('/api/plugins/com.example%2Fzhibao');
  });

  it('sends a local path as the source the kernel resolves on its own machine', () => {
    expect(installLocalPathOperation(' /srv/neige/plugins/todo ').body)
      .toEqual({ source: { kind: 'local_path', path: '/srv/neige/plugins/todo' } });
  });
});
