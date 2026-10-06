import { describe, expect, it } from 'vitest';
import { z } from 'zod';

import { performApiRequest } from '../api/client.js';
import { ApiError, NotSentError } from './failure-class.js';
import { probeFailureText } from './read-failure.js';
import {
  EMPTY_CONNECTOR_DRAFT, PLUGIN_CONFIG_TEXT, configDraftFrom, configFieldsOf, configPatchFrom, configWriteError,
  CONNECTOR_CHECK_TEXT, connectorDraftError, installConnectorOperation, installLocalPathOperation, toolsAllowOf,
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
  it('requires plugin configuration and removal capabilities', () => {
    const row = {
      id: 'git-forge', version: '0.1.0', enabled: true, state: 'running', manifest_name: 'Git forge',
    };
    expect(pluginListItemSchema.safeParse(row).success).toBe(false);
    expect(pluginListItemSchema.safeParse({ ...row, has_config: false }).success).toBe(false);
    expect(pluginListItemSchema.safeParse({ ...row, has_config: false, can_uninstall: true, can_disable: true }).success).toBe(true);
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

/** A rejection as the hooks carry it: the kernel's `ErrorBody` answer, or a lost one. */
const answered = (status: number, code: string, message: string) => new ApiError({ kind: 'http', status, code, message, body: { error: message, code } });
const lost = new ApiError({ kind: 'transport', message: 'Transport request failed' });

/**
 * A rejection as the client hands it over for the kernel's exact `ErrorBody`: `body` is what the server sends
 * (`plugin_routes.rs` pins these bodies), normalized by the real client.
 */
async function kernelRefusal(status: number, body: Readonly<Record<string, string>>): Promise<ApiError> {
  const result = await performApiRequest(
    { send: () => Promise.resolve({ status, statusText: '', body }) },
    { method: 'PATCH', path: '/api/plugins/git-forge/config', body: {}, responseSchema: z.unknown() },
  );
  if (result.status !== 'failed') throw new Error('the kernel answer must be a failure');
  return new ApiError(result.error);
}

const INTEGER_REASON = 'expected type `integer` (an integer-encoded JSON number; float-encoded values such as `1.0` are rejected)';

describe('configWriteError', () => {
  const fields = configFieldsOf(schema());

  it('puts a schema violation on the field the kernel named', async () => {
    const error = configWriteError(
      await kernelRefusal(400, { error: INTEGER_REASON, code: 'bad_request', field: 'config.retries' }), fields,
    );
    expect(error).toEqual({ message: INTEGER_REASON, fieldKey: 'retries', offersReset: false });
  });

  it('lands a violation on the declared key it names, not on one that starts the same', async () => {
    const pair = configFieldsOf({
      type: 'object',
      properties: { token: { type: 'string' }, token_extra: { type: 'string' } },
    });
    const violation = (field: string) => kernelRefusal(400, { error: 'expected type `string`', code: 'bad_request', field });
    expect(configWriteError(await violation('config.token_extra'), pair).fieldKey).toBe('token_extra');
    expect(configWriteError(await violation('config.token'), pair).fieldKey).toBe('token');
  });

  it('offers the reset for the byte-cap refusal too, without reading the prose', async () => {
    const tooLarge = configWriteError(
      await kernelRefusal(400, {
        error: 'config: storing this patch would make plugin `git-forge`\'s user_config 40000 '
          + 'bytes, over the 32768-byte cap. Resend this request with `?reset=true`',
        code: 'plugin_config_too_large',
      }),
      fields,
    );
    expect(tooLarge.offersReset).toBe(true);
    expect(tooLarge.fieldKey).toBeNull();
    expect(tooLarge.message).toContain('32768');

    const passing = await kernelRefusal(400, { error: 'something mentioning ?reset=true in passing', code: 'bad_request' });
    expect(configWriteError(passing, fields).offersReset).toBe(false);
  });

  it('keeps a violation of an undeclared key off the form, naming the key on the pane', async () => {
    const error = configWriteError(
      await kernelRefusal(400, {
        error: 'unknown field (schema declares additionalProperties: false)', code: 'bad_request', field: 'config.ghost',
      }),
      fields,
    );
    expect(error).toEqual({
      message: 'config.ghost: unknown field (schema declares additionalProperties: false)', fieldKey: null, offersReset: false,
    });
  });

  it('shows a refusal of the whole patch on the pane in the kernel’s words', async () => {
    const error = configWriteError(
      await kernelRefusal(400, { error: 'config: must serialize to at most 8192 bytes', code: 'bad_request' }), fields,
    );
    expect(error).toEqual({ message: 'config: must serialize to at most 8192 bytes', fieldKey: null, offersReset: false });
  });

  it('shows a held lock in the kernel’s words, as a refusal like any other', () => {
    const error = configWriteError(answered(409, 'plugin_busy', 'plugin `git-forge` is busy'), fields);
    expect(error).toEqual({ message: 'plugin `git-forge` is busy', fieldKey: null, offersReset: false });
  });

  it('offers the reset only for the refusal whose exit it is', () => {
    const corrupt = configWriteError(answered(409, 'plugin_config_corrupt', 'stored user_config is not a JSON object'), fields);
    expect(corrupt.offersReset).toBe(true);
    expect(corrupt.message).toContain('not a JSON object');

    const unloaded = configWriteError(
      answered(409, 'plugin_manifest_unloaded', 'manifest is not loaded in the kernel registry; reload the plugin'), fields,
    );
    expect(unloaded.offersReset).toBe(false);
    expect(unloaded.message).toContain('reload the plugin');
  });

  it('shows the fixed sentence for an outcome it cannot know, and the fixed refusal for a write not sent', () => {
    for (const error of [lost, answered(500, 'db_error', 'locked'), new Error('stale intent')]) {
      expect(configWriteError(error, fields)).toEqual({ message: PLUGIN_CONFIG_TEXT.unknown, fieldKey: null, offersReset: false });
    }
    expect(configWriteError(new NotSentError(), fields).message).toBe(PLUGIN_CONFIG_TEXT.refused);
  });
});

describe('reloadOutcome (#1284 §2.4)', () => {
  it('reports a refusal before the stop as saved-but-not-restarted, in the kernel’s words', () => {
    const outcome = reloadOutcome({ rejection: { error: answered(409, 'plugin_busy', 'plugin `git-forge` is busy') }, state: 'running' });
    expect(outcome.kind).toBe('refused');
    expect(outcome.tone).toBe('warning');
    expect(outcome.message).toMatch(/saved/i);
    expect(outcome.message).toMatch(/did not run, so the plugin keeps the configuration it last started with/);
    expect(outcome.message).toContain('plugin `git-forge` is busy');
  });

  it.each(['spawning', 'unknown', 'disabled', 'crashed'] as const)(
    'reads a busy or missing plugin as refused before the stop, whatever reads back (%s)', (state) => {
      for (const error of [answered(409, 'plugin_busy', 'plugin `git-forge` is busy'), answered(404, 'not_found', 'plugin git-forge')]) {
        const outcome = reloadOutcome({ rejection: { error }, state });
        expect(outcome.kind).toBe('refused');
        expect(outcome.message).not.toMatch(/has stopped|still running/);
      }
    },
  );

  it('reads a refusal after the stop as stopped even when something else brought the plugin up', () => {
    expect(reloadOutcome({ rejection: { error: answered(409, 'plugin_conflict', 'template taken') }, state: 'running' }).kind).toBe('stopped');
  });

  it('says a restart that was never sent did not run, rather than that the plugin stopped', () => {
    const outcome = reloadOutcome({ rejection: { error: new NotSentError() }, state: 'unknown' });
    expect(outcome.kind).toBe('refused');
    expect(outcome.message).not.toMatch(/has stopped/);
  });

  it('carries last_error verbatim when the plugin landed in unavailable', () => {
    /* `unavailable` is a connector's normal terminal state, not a kernel error; `last_error` is the only diagnostic. */
    const reason = 'mcp-http: connect to https://api.example.com failed: connection refused';
    const outcome = reloadOutcome({ rejection: { error: answered(400, 'bad_request', 'reload failed') }, state: 'unavailable', lastError: reason });
    expect(outcome.kind).toBe('unavailable');
    expect(outcome.message).toContain(reason);
  });

  it('reads unavailable off the state even when the reload answered 200', () => {
    const outcome = reloadOutcome({ rejection: null, state: 'unavailable', lastError: 'upstream said no' });
    expect(outcome.kind).toBe('unavailable');
    expect(outcome.message).toContain('upstream said no');
  });

  it('says an app the kernel refused after the stop has stopped', () => {
    const outcome = reloadOutcome({
      rejection: { error: answered(400, 'plugin_install', 'spawn failed: No such file or directory') }, state: 'installed',
    });
    expect(outcome.kind).toBe('stopped');
    expect(outcome.message).toMatch(/stopped/);
    expect(outcome.message).toContain('spawn failed: No such file or directory');
    expect(reloadOutcome({ rejection: { error: answered(400, 'plugin_install', 'bad manifest') }, state: 'unknown' }).kind).toBe('stopped');
  });

  it('confirms only when something is actually running the new configuration', () => {
    expect(reloadOutcome({ rejection: null, state: 'running' })).toMatchObject({
      kind: 'applied', tone: 'success',
    });
    expect(reloadOutcome({ rejection: null, state: 'spawning' })).toMatchObject({
      kind: 'starting', tone: 'success',
    });
    const idle = reloadOutcome({ rejection: null, state: 'disabled' });
    expect(idle.kind).toBe('idle');
    expect(idle.tone).toBe('warning');
    expect(idle.message).toMatch(/enable/i);
  });

  it('says the state is unknown when nothing confirmed the restart, never naming the connection', () => {
    for (const error of [lost, answered(500, 'internal', 'stop failed: timeout'), new Error('stale intent')]) {
      const outcome = reloadOutcome({ rejection: { error }, state: 'unknown' });
      expect(outcome.kind).toBe('unknown');
      expect(outcome.tone).toBe('warning');
      expect(outcome.message).toMatch(/unknown/);
      expect(outcome.message).toMatch(/saved/i);
      expect(outcome.message).not.toMatch(/has stopped|connection|Transport request failed|stop failed/);
    }
  });

  it('does not confirm a restart it cannot know, even when the plugin reads back running', () => {
    /* A 500 can be a stop that failed: the plugin still runs its previous configuration. */
    const outcome = reloadOutcome({ rejection: { error: answered(500, 'internal', 'stop failed: timeout') }, state: 'running' });
    expect(outcome.kind).toBe('unknown');
    expect(outcome.message).not.toMatch(/restarted with it/);
  });

  it('paints unavailable as a warning rather than an error', () => {
    expect(reloadOutcome({ rejection: null, state: 'unavailable', lastError: 'upstream said no' }).tone)
      .toBe('warning');
    expect(reloadOutcome({ rejection: null, state: 'unavailable' }).tone).toBe('warning');
  });
});

describe('a failed connector check', () => {
  it('shows the kernel’s account of the upstream server, else a fixed sentence', () => {
    expect(probeFailureText(answered(502, 'mcp_setup_failed', 'HTTP 401: authentication failed'), CONNECTOR_CHECK_TEXT))
      .toBe('HTTP 401: authentication failed');
    expect(probeFailureText(answered(502, 'mcp_setup_failed', ''), CONNECTOR_CHECK_TEXT)).toBe('The connection check failed.');
    for (const error of [lost, new ApiError({ kind: 'decode', message: 'API response did not match its schema' }), new Error('x')]) {
      expect(probeFailureText(error, CONNECTOR_CHECK_TEXT)).toBe('The connection check could not finish. Try again.');
    }
  });

  it('names the field a refusal is about', () => {
    expect(probeFailureText(new ApiError({ kind: 'http', status: 400, code: 'bad_request', field: 'url', message: 'must use https' }),
      CONNECTOR_CHECK_TEXT)).toBe('url: must use https');
  });

  /* The session's 401 is not the upstream server refusing the connector's credentials, which the check answers as 502. */
  it('reads a signed-out session as a check that never ran', () => {
    expect(probeFailureText(new ApiError({ kind: 'unauthorized', status: 401, code: 'unauthorized', message: 'unauthorized' }),
      CONNECTOR_CHECK_TEXT)).toBe('The connection check could not finish. Try again.');
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

it('rejects an otherwise complete legacy row missing lifecycle permission', () => {
  expect(pluginListItemSchema.safeParse({ id: 'x', version: '1', enabled: true, state: 'running', manifest_name: 'X', has_config: false, can_uninstall: true }).success).toBe(false);
});
