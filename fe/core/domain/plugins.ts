// Installed plugins: the list Settings › Plugins reads, and the lifecycle writes it offers.

import { z } from 'zod';

import type { ApiOperation } from '../api/types.js';
import type { McpCheckResult } from '../api/generated/wire.js';
import {
  NotSentError, classifyFailure, refusalText, writeFailureOf,
  type FailureTable, type WriteClass, type WriteText,
} from './failure-class.js';
import type { ProbeText } from './read-failure.js';

/**
 * The kernel's wire-name set for a plugin's runtime state. `unavailable` is a connector's normal
 * terminal state, not a kernel error. `catch`-guarded: an unknown wire name degrades to `unknown`.
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
  /** Required, not defaulted: a kernel that did not send it is not the one this screen was written against. */
  has_config: z.boolean(),
  can_uninstall: z.boolean(),
  can_disable: z.boolean(),
});
export type PluginListItem = z.infer<typeof pluginListItemSchema>;

export function pluginsOperation(): ApiOperation<PluginListItem[]> {
  return { method: 'GET', path: '/api/plugins', responseSchema: z.array(pluginListItemSchema) };
}

/*
 * What a failed plugin write means (#2131), read through `writeFailureText`: a refusal shows the kernel's reason at
 * the plugin, `done` shows nothing, `unknown` shows the write's fixed sentence. 409 `plugin_busy` touched nothing and
 * is a refusal like any other. Every write re-reads the list (and the configuration pane its plugin) when it settles.
 */

/**
 * `POST /api/plugins/{id}/enable|disable`, which set the state they name. Refusals leave the plugin as they found it:
 * 400 (an always-on built-in), 404, 409 `plugin_busy` or `plugin_conflict`, and 422 (kernel too old). A 503
 * `service_unavailable` is an enable that landed while the plugin waits on its connector or a required key, so it is
 * `done`: the re-read list shows that reason as the plugin's `last_error`. A 500 may follow a partial change, so it is
 * `unknown`.
 */
export const PLUGIN_TOGGLE_FAILURES: FailureTable<WriteClass> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([503]), code: 'service_unavailable', is: 'done' as const }),
    Object.freeze({ status: Object.freeze([400, 404, 409, 422]), is: 'refused' as const }),
  ]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

export const PLUGIN_TOGGLE_TEXT: WriteText = Object.freeze({
  refused: 'The plugin was not changed.',
  unknown: 'The change is unconfirmed. The switch shows what is in effect.',
});

/** `DELETE /api/plugins/{id}`: the shape of `DELETE_FAILURES` (404 is `done`), plus 400 for a built-in. */
export const PLUGIN_UNINSTALL_FAILURES: FailureTable<WriteClass> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([404]), is: 'done' as const }),
    Object.freeze({ status: Object.freeze([400, 403, 409]), is: 'refused' as const }),
  ]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

/**
 * `POST /api/plugins/install`, a first attempt. Refusals: 400 `plugin_install` (and the extractor's 4xx), 409
 * `plugin_conflict` (the id is already installed), `plugin_dir_occupied` (a directory the kernel did not write holds the
 * id's place) or `plugin_busy`, 422. A retry after an `unknown` answer reads {@link PLUGIN_INSTALL_RETRY_FAILURES}.
 */
export const PLUGIN_INSTALL_FAILURES: FailureTable<WriteClass> = Object.freeze({
  rules: Object.freeze([Object.freeze({ status: Object.freeze([400, 409, 413, 415, 422]), is: 'refused' as const })]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

/**
 * The same install after an `unknown` answer: "already installed" (`plugin_conflict`) is the earlier attempt having
 * landed, so `done`. `plugin_dir_occupied` installed nothing and stays a refusal.
 */
export const PLUGIN_INSTALL_RETRY_FAILURES: FailureTable<WriteClass> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([409]), code: 'plugin_conflict', is: 'done' as const }),
    Object.freeze({ status: Object.freeze([400, 409, 413, 415, 422]), is: 'refused' as const }),
  ]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

export const PLUGIN_INSTALL_TEXT: WriteText = Object.freeze({
  refused: 'The plugin was not added.',
  unknown: 'The add is unconfirmed. Adding the plugin again is safe.',
});

/**
 * `PATCH /api/plugins/{id}/config`, a merge patch that is safe to repeat. Refusals: 400 (a schema violation, or
 * `plugin_config_too_large`), 404, 409 (`plugin_busy`, `plugin_manifest_unloaded`, `plugin_config_corrupt`), and
 * the extractor's 413/415/422.
 */
export const PLUGIN_CONFIG_FAILURES: FailureTable<WriteClass> = Object.freeze({
  rules: Object.freeze([Object.freeze({ status: Object.freeze([400, 404, 409, 413, 415, 422]), is: 'refused' as const })]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

export const PLUGIN_CONFIG_TEXT: WriteText = Object.freeze({
  refused: 'Nothing was saved.',
  unknown: 'The save is unconfirmed. Saving again is safe.',
});

/** The two config refusals whose stated exit is `?reset=true`; the message text is never read for it. */
export const PLUGIN_CONFIG_RESET_OFFERS: FailureTable<'reset' | 'none'> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([409]), code: 'plugin_config_corrupt', is: 'reset' as const }),
    Object.freeze({ status: Object.freeze([400]), code: 'plugin_config_too_large', is: 'reset' as const }),
  ]),
  unauthorized: 'none',
  otherwise: 'none',
});

/**
 * The reload answers given before anything stopped (`lifecycle.rs` `reload`): 404 from the existence probe and 409
 * `plugin_busy` from the lifecycle lock. Every other answer may follow the stop.
 */
export const PLUGIN_RELOAD_BEFORE_STOP: FailureTable<'untouched' | 'after-stop'> = Object.freeze({
  rules: Object.freeze([
    Object.freeze({ status: Object.freeze([404]), is: 'untouched' as const }),
    Object.freeze({ status: Object.freeze([409]), code: 'plugin_busy', is: 'untouched' as const }),
  ]),
  unauthorized: 'untouched',
  otherwise: 'after-stop',
});

/**
 * `POST /api/plugins/{id}/reload`. Refusals: 404 and 409 `plugin_busy` before anything stopped; 400 (the manifest),
 * 409 `plugin_conflict`, 422 and 503 after the stop. A 500 may have stopped it or not, so it is `unknown`. Which of
 * these left the plugin where is read back from its state by {@link reloadOutcome}.
 */
export const PLUGIN_RELOAD_FAILURES: FailureTable<WriteClass> = Object.freeze({
  rules: Object.freeze([Object.freeze({ status: Object.freeze([400, 404, 409, 422, 503]), is: 'refused' as const })]),
  unauthorized: 'refused',
  otherwise: 'unknown',
});

/** Enable / disable, as one operation taking the target state. */
export function setPluginEnabledOperation(id: string, enabled: boolean): ApiOperation<{ id: string; enabled: boolean }> {
  return {
    method: 'POST',
    path: `/api/plugins/${encodeURIComponent(id)}/${enabled ? 'enable' : 'disable'}`,
    responseSchema: z.object({ id: z.string(), enabled: z.boolean() }).loose(),
  };
}

/**
 * Where an `mcp-http` connector's API key rides: `bearer` sends `Authorization: Bearer <key>`,
 * `header` the bare key under a named header.
 */
export type ApiKeyPlacement = 'bearer' | 'header';
export type ConnectorToolMode = 'all' | 'selected';

/**
 * What the operator fills in to add a remote MCP server. `api_key` travels only in this request
 * body; every field is validated by the kernel, and this module does not restate those rules.
 */
export type ConnectorInstallDraft = Readonly<{
  id: string;
  display_name: string;
  description: string;
  url: string;
  headers: Readonly<Record<string, string>>;
  api_key: string;
  placement: ApiKeyPlacement;
  header_name: string;
  /**
   * `all` discovers the server's complete catalog on every enable/reload; `selected` sends an
   * explicit strict allowlist.
   */
  tool_mode: ConnectorToolMode;
  /**
   * In `selected` mode, the upstream tools as typed (commas, spaces or newlines); an empty list is
   * invalid in this form.
   */
  tools: string;
}>;

export const EMPTY_CONNECTOR_DRAFT: ConnectorInstallDraft = Object.freeze({
  id: '', display_name: '', description: '', url: '', api_key: '',
  placement: 'bearer', header_name: '', tool_mode: 'all', tools: '', headers: Object.freeze({}),
});

/** The tool names in a draft, in the order typed, deduplicated. */
export function toolsAllowOf(draft: ConnectorInstallDraft): string[] {
  return [...new Set(draft.tools.split(/[\s,]+/).filter((name) => name !== ''))];
}

/** The `api_key_in` wire string, or `null` for `header` with no header name (incomplete, not defaulted). */
export function apiKeyInOf(draft: ConnectorInstallDraft): string | null {
  if (draft.placement === 'bearer') return 'bearer';
  const name = draft.header_name.trim();
  return name === '' ? null : `header:${name}`;
}

/** The one refusal this screen makes on its own, or `null`; every judgement belongs to the kernel. */
export function connectorDraftError(draft: ConnectorInstallDraft): string | null {
  if (draft.id.trim() === '') return 'An id is required.';
  if (draft.display_name.trim() === '') return 'A name is required.';
  if (draft.url.trim() === '') return 'A server URL is required.';
  if (draft.api_key.trim() !== '' && apiKeyInOf(draft) === null) {
    return 'A header name is required when the key rides in a custom header.';
  }
  if (draft.tool_mode === 'selected' && toolsAllowOf(draft).length === 0) {
    return 'Name at least one tool to expose — a connector with none exposes nothing.';
  }
  return null;
}

/**
 * The install response decoded down to its identity; every other field is read from the list this
 * install invalidates.
 */
export const installedPluginSchema = z.object({ id: z.string(), enabled: z.boolean() }).loose();
export type InstalledPlugin = z.infer<typeof installedPluginSchema>;

/**
 * `POST /api/plugins/install` with `source.kind = "mcp_http_v2"`. A blank credential is sent as an
 * absent key, not `""`: the kernel reads absent as unauthenticated and refuses an empty string.
 */
export function installConnectorOperation(draft: ConnectorInstallDraft): ApiOperation<InstalledPlugin> {
  const key = draft.api_key.trim();
  const description = draft.description.trim();
  return {
    method: 'POST',
    path: '/api/plugins/install',
    body: {
      source: {
        kind: 'mcp_http_v2',
        id: draft.id.trim(),
        display_name: draft.display_name.trim(),
        ...(description === '' ? {} : { description }),
        url: draft.url.trim(),
        ...(Object.keys(draft.headers).length === 0 ? {} : { headers: draft.headers }),
        ...(draft.tool_mode === 'selected' ? { tools_allow: toolsAllowOf(draft) } : { tools_all: true }),
        ...(key === '' ? {} : { api_key: key, api_key_in: apiKeyInOf(draft) }),
      },
    },
    responseSchema: installedPluginSchema,
  };
}

/**
 * `POST /api/plugins/mcp/check` is a read-only probe, read by `probeFailureText`: an answered failure is the kernel's
 * account of the upstream server (400/502 `mcp_setup_failed`).
 */
export const CONNECTOR_CHECK_TEXT: ProbeText = Object.freeze({
  answered: 'The connection check failed.', unfinished: 'The connection check could not finish. Try again.',
});

/** Check is a transient POST; its body must never become a query key/cache. */
export const connectorCheckSchema: z.ZodType<McpCheckResult> = z.object({ tools: z.array(z.string()) });
export type ConnectorCheckResult = Readonly<{ ok: true; tools: readonly string[] }>
  | Readonly<{ ok: false; message: string }>;

export function checkConnectorOperation(draft: ConnectorInstallDraft): ApiOperation<z.infer<typeof connectorCheckSchema>> {
  const source = (installConnectorOperation(draft).body as { source: Record<string, unknown> }).source;
  const body = { ...source };
  delete body.kind;
  return { method: 'POST', path: '/api/plugins/mcp/check', body, responseSchema: connectorCheckSchema };
}

/**
 * `POST /api/plugins/install` with `source.kind = "local_path"`: a directory on the server,
 * resolved in the kernel's filesystem.
 */
export function installLocalPathOperation(path: string): ApiOperation<InstalledPlugin> {
  return {
    method: 'POST',
    path: '/api/plugins/install',
    body: { source: { kind: 'local_path', path: path.trim() } },
    responseSchema: installedPluginSchema,
  };
}

/** `DELETE /api/plugins/{id}`. Destructive: a connector loses its stored credential with the tree. */
export function uninstallPluginOperation(id: string): ApiOperation<undefined> {
  return {
    method: 'DELETE',
    path: `/api/plugins/${encodeURIComponent(id)}`,
    responseSchema: z.undefined(),
  };
}

/**
 * `GET /api/plugins/{id}`. `config_schema` is the top-level registry field, never
 * `manifest.config_schema`. `user_config` is `unknown` because the kernel keeps a non-object
 * value (409 `plugin_config_corrupt`) rather than coercing it away.
 */
export const pluginDetailSchema = z.object({
  id: z.string(),
  version: z.string(),
  enabled: z.boolean(),
  state: pluginStateSchema,
  last_error: z.string().optional(),
  config_schema: z.unknown().optional(),
  user_config: z.unknown(),
  effective_config: z.unknown(),
});
export type PluginDetail = z.infer<typeof pluginDetailSchema>;

export function pluginDetailOperation(id: string): ApiOperation<PluginDetail> {
  return {
    method: 'GET',
    path: `/api/plugins/${encodeURIComponent(id)}`,
    responseSchema: pluginDetailSchema,
  };
}

/**
 * `PATCH /api/plugins/{id}/config`. `patch` must carry only keys the operator changed: the
 * kernel applies defaults on read and never stores them. Absent means unchanged; `null` deletes.
 * `reset` discards the stored document first and is never sent implicitly.
 */
export function patchPluginConfigOperation(
  id: string,
  patch: Readonly<Record<string, PluginConfigValue | null>>,
  options: Readonly<{ reset: boolean }> = { reset: false },
): ApiOperation<PluginDetail> {
  const query = options.reset ? '?reset=true' : '';
  return {
    method: 'PATCH',
    path: `/api/plugins/${encodeURIComponent(id)}/config${query}`,
    body: patch,
    responseSchema: pluginDetailSchema,
  };
}

/**
 * `POST /api/plugins/{id}/reload`: stop, re-read the manifest, start again; the response is the
 * detail *after* the attempt.
 */
export function reloadPluginOperation(id: string): ApiOperation<PluginDetail> {
  return {
    method: 'POST',
    path: `/api/plugins/${encodeURIComponent(id)}/reload`,
    responseSchema: pluginDetailSchema,
  };
}

/** Every value a `config_schema` property can hold in the kernel's subset. */
export type PluginConfigValue = string | number | boolean;

/** The four property types the kernel accepts; `enum` is a constraint on a string field, not a fifth type. */
export type PluginConfigFieldKind = 'string' | 'integer' | 'number' | 'boolean';

export type PluginConfigField = Readonly<{
  /** The key as declared; also the row's label, since it is the name the manifest and API use. */
  key: string;
  kind: PluginConfigFieldKind;
  /** Non-empty ⇒ the field is a choice, and these are the choices. */
  options: readonly string[];
  description: string | null;
  /** The manifest's default, for display only: a placeholder, never pre-filled and never in a payload. */
  default: PluginConfigValue | null;
  required: boolean;
}>;

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function asConfigValue(value: unknown, kind: PluginConfigFieldKind): PluginConfigValue | null {
  if (kind === 'boolean') return typeof value === 'boolean' ? value : null;
  if (kind === 'string') return typeof value === 'string' ? value : null;
  return typeof value === 'number' && Number.isFinite(value) ? value : null;
}

/**
 * The controls a `config_schema` asks for. A property outside the kernel's subset is dropped
 * rather than guessed at, and a schema that is not the subset at all yields `[]`; never throws.
 */
export function configFieldsOf(schema: unknown): readonly PluginConfigField[] {
  if (!isPlainObject(schema)) return [];
  const properties = schema.properties;
  if (!isPlainObject(properties)) return [];
  const required = Array.isArray(schema.required)
    ? schema.required.filter((key): key is string => typeof key === 'string')
    : [];
  const fields: PluginConfigField[] = [];
  for (const [key, property] of Object.entries(properties)) {
    if (!isPlainObject(property)) continue;
    const kind = property.type;
    if (kind !== 'string' && kind !== 'integer' && kind !== 'number' && kind !== 'boolean') continue;
    const options = Array.isArray(property.enum)
      ? property.enum.filter((option): option is string => typeof option === 'string')
      : [];
    fields.push({
      key,
      kind,
      options,
      description: typeof property.description === 'string' ? property.description : null,
      default: asConfigValue(property.default, kind),
      required: required.includes(key),
    });
  }
  return fields;
}

/** The stored document, or `null` for the corrupt row the kernel refuses to merge into — distinct from `{}`. */
export function storedConfigOf(userConfig: unknown): Readonly<Record<string, unknown>> | null {
  return isPlainObject(userConfig) ? userConfig : null;
}

/** What each control holds. `null` is "no value" — empty field, cleared choice. */
export type PluginConfigDraft = Readonly<Record<string, PluginConfigValue | null>>;

/**
 * What the controls start at. The baseline is `user_config`, not `effective_config`, so a
 * manifest default is never posted back. A switch has no third position, so a boolean with no
 * stored value starts at its default; a mistyped stored value starts empty and is left alone.
 */
export function configDraftFrom(
  fields: readonly PluginConfigField[],
  stored: Readonly<Record<string, unknown>> | null,
): PluginConfigDraft {
  const draft: Record<string, PluginConfigValue | null> = {};
  for (const field of fields) {
    const value = stored === null ? null : asConfigValue(stored[field.key], field.kind);
    draft[field.key] = field.kind === 'boolean' ? (value ?? field.default ?? false) : value;
  }
  return draft;
}

/**
 * The patch body for a Save: only keys whose control moved; a cleared key is `null`; a manifest
 * default is never written. For a boolean, the value that equals the declared default means
 * "inherit" and the patch says `null`, since a switch cannot send "unset".
 */
export function configPatchFrom(
  fields: readonly PluginConfigField[],
  base: PluginConfigDraft,
  draft: PluginConfigDraft,
): Readonly<Record<string, PluginConfigValue | null>> {
  const patch: Record<string, PluginConfigValue | null> = {};
  for (const field of fields) {
    const next = draft[field.key] ?? null;
    const previous = base[field.key] ?? null;
    if (next === previous) continue;
    patch[field.key] = field.kind === 'boolean' && next === field.default ? null : next;
  }
  return patch;
}

/** What one Save did; a failure carries the write's rejection as thrown, read by `configWriteError`. */
export type PluginConfigSaveResult =
  | Readonly<{ ok: true }>
  | Readonly<{ ok: false; error: unknown }>;

/** The facts read after a restart attempt; `reloadOutcome` is the only place they become a sentence. */
export type PluginRestartFacts = Readonly<{
  /** The reload's rejection as thrown, or `null` when it answered 2xx. */
  rejection: Readonly<{ error: unknown }> | null;
  state: PluginState;
  lastError?: string;
}>;

/** What one Apply & restart did: never got past the write, or restarted and left the plugin somewhere. */
export type PluginConfigApplyResult =
  | Readonly<{ saved: false; error: unknown }>
  | Readonly<{ saved: true; restart: PluginRestartFacts }>;

export type PluginConfigWriteError = Readonly<{
  /** The sentence to show; the kernel's own wording wherever it refused. */
  message: string;
  /** The declared key the refusal's `field` named, when this form renders it; `null` puts the message on the pane. */
  fieldKey: string | null;
  /** Whether `?reset=true` is the kernel's stated exit from this refusal. */
  offersReset: boolean;
}>;

/** The root every config field path starts with: the kernel names a violation of key `k` as field `config.k`. */
const CONFIG_FIELD_ROOT = 'config.';

/**
 * A rejected `PATCH /config`, read through {@link PLUGIN_CONFIG_FAILURES}: an unknown outcome is the fixed sentence;
 * a refusal is the kernel's reason, on the field its `field` names when this form renders that key (else on the pane,
 * after the field's path), with the `?reset=true` offer where {@link PLUGIN_CONFIG_RESET_OFFERS} says it is the exit.
 */
export function configWriteError(error: unknown, fields: readonly PluginConfigField[]): PluginConfigWriteError {
  const failure = writeFailureOf(error);
  const refusal = refusalText(failure, PLUGIN_CONFIG_FAILURES, PLUGIN_CONFIG_TEXT.refused);
  if (refusal === null) return { message: PLUGIN_CONFIG_TEXT.unknown, fieldKey: null, offersReset: false };
  const answered = failure !== null && !(failure instanceof NotSentError) && failure.kind === 'http' ? failure : null;
  if (answered?.field !== undefined) {
    /* Placed on its control, the reason is shown alone; any other field stays in the refusal's sentence. */
    const { field } = answered;
    const key = field.startsWith(CONFIG_FIELD_ROOT) ? field.slice(CONFIG_FIELD_ROOT.length) : null;
    return key !== null && fields.some((candidate) => candidate.key === key)
      ? { message: answered.message, fieldKey: key, offersReset: false }
      : { message: refusal, fieldKey: null, offersReset: false };
  }
  const offersReset = !(failure instanceof NotSentError) && classifyFailure(failure, PLUGIN_CONFIG_RESET_OFFERS) === 'reset';
  return { message: refusal, fieldKey: null, offersReset };
}

/** What a reload attempt actually did; `unknown` is the ending where nothing confirmed the restart. */
export type PluginReloadOutcomeKind =
  | 'applied' | 'starting' | 'refused' | 'unavailable' | 'stopped' | 'idle' | 'unknown';

export type PluginReloadOutcome = Readonly<{
  kind: PluginReloadOutcomeKind;
  message: string;
  /** `unavailable` and `stopped` are warnings about the plugin, not errors of the kernel. */
  tone: 'success' | 'warning';
}>;

/**
 * The status code is not the verdict: a reload stops the plugin before re-reading anything, a connector whose
 * bring-up fails ends in `unavailable` + `last_error`, and a refusal before the stop (`plugin_busy`) touched nothing.
 * The rejection is read through {@link PLUGIN_RELOAD_FAILURES}; the plugin's state read back afterwards says where
 * a refusal left it.
 */
export function reloadOutcome(facts: PluginRestartFacts): PluginReloadOutcome {
  const { rejection, state, lastError } = facts;
  const failure = rejection === null ? null : writeFailureOf(rejection.error);
  const refusal = rejection === null ? null : refusalText(failure, PLUGIN_RELOAD_FAILURES, '');
  /* Never sent, or refused before the stop: nothing stopped the plugin, whatever its state reads back as. */
  if (refusal !== null && (failure instanceof NotSentError || classifyFailure(failure, PLUGIN_RELOAD_BEFORE_STOP) === 'untouched')) {
    return {
      kind: 'refused',
      tone: 'warning',
      message: `Configuration saved. The restart did not run, so the plugin keeps the configuration it last started with. ${refusal}`.trim(),
    };
  }
  if (state === 'unavailable') {
    /* `last_error` verbatim: it is the kernel's only account of why a bring-up failed. */
    return {
      kind: 'unavailable',
      tone: 'warning',
      message: lastError === undefined
        ? 'Configuration saved. The plugin did not come up with it, and the kernel recorded no reason.'
        : `Configuration saved. The plugin did not come up with it: ${lastError}`,
    };
  }
  if (rejection !== null && refusal === null) {
    /* Nothing confirmed what the restart did, so neither "stopped" nor "still up" is stated. */
    return {
      kind: 'unknown',
      tone: 'warning',
      message: 'Configuration saved. The restart is unconfirmed, so this plugin\'s current state is unknown. '
        + 'Reload this screen to see where it is.',
    };
  }
  if (refusal !== null || state === 'crashed') {
    return {
      kind: 'stopped',
      tone: 'warning',
      message: `The plugin has stopped and did not start with the new configuration. ${refusal ?? lastError ?? ''}`.trim(),
    };
  }
  if (state === 'spawning' || state === 'installing') {
    return { kind: 'starting', tone: 'success', message: 'Configuration saved. The plugin is starting with it.' };
  }
  if (state === 'running') {
    return { kind: 'applied', tone: 'success', message: 'Configuration saved and the plugin restarted with it.' };
  }
  /* A disabled (or never-started) plugin re-reads its manifest and stays where it is; no process
     holds the configuration. */
  return {
    kind: 'idle',
    tone: 'warning',
    message: 'Configuration saved. This plugin is not running, so enable it to use the new configuration.',
  };
}
