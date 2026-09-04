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
  /**
   * #1284 §2.5 — does this plugin declare a `config_schema`?
   *
   * **Required, not optional-with-a-default.** The whole point of the bit is
   * that "this plugin has nothing to configure" and "the configuration screen
   * is not built" are different things on screen, and a decoder that invents
   * `false` for a kernel that did not send it would answer the first question
   * with a guess. The kernel serializes it unconditionally
   * (`routes/plugins.rs::PluginListItem`), so a missing key means the response
   * is not the one this screen was written against, and a decode failure says
   * so.
   */
  has_config: z.boolean(),
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

// ---------------------------------------------------------------------------
// One plugin's detail, and its configuration (#1284 S4)
// ---------------------------------------------------------------------------

/**
 * `GET /api/plugins/{id}`, decoded down to what the configuration screen needs.
 *
 * `config_schema` is the **top-level** field and never `manifest.config_schema`
 * (#1284 F17): the persisted `manifest` blob is published verbatim as the row
 * it is, so on any kernel installed before #1284 it simply has no
 * `config_schema` key — serde dropped it at install time. The top-level field
 * comes off the registry, which is the document the `PATCH` validates against
 * and every runtime consumer reads. Rendering a form from the blob would mean
 * offering controls the write path does not agree exist. The blob is therefore
 * not decoded here at all: nothing on this screen renders it, and keeping it
 * out is what makes the wrong copy unreachable rather than merely unused.
 *
 * `user_config` and `effective_config` are both `unknown` on purpose. They are
 * whatever the kernel stored — including, for `user_config`, a value that is
 * not an object at all, which is a state the kernel explicitly keeps (409
 * `plugin_config_corrupt`) rather than coercing away. A schema that demanded an
 * object here would turn that row into a decode failure, i.e. into a blank
 * screen with no way to reach the one endpoint that can repair it. See
 * [`storedConfigOf`].
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
 * `PATCH /api/plugins/{id}/config` — the keys being edited, and nothing else.
 *
 * **`patch` must carry only keys the operator changed** (#1284 §2.2.5). The
 * kernel applies defaults on *read* and never stores them (§2.2.4), so a form
 * that posted its whole effective object back would materialize every default
 * into the row on the first Save — after which a manifest that later changes a
 * default would never again reach that plugin, because the row now holds an
 * explicit copy of the old one. Absent means unchanged; an explicit `null`
 * deletes the key. [`configPatchFrom`] is what builds such a patch.
 *
 * `reset` discards the stored document before applying the patch. It is the
 * kernel's own exit from two refusals — a `user_config` that is not an object
 * (409 `plugin_config_corrupt`) and a row whose total size is over the cap
 * because of residue no ordinary patch can shrink — and it is destructive, so
 * it is never sent implicitly: a caller passes it because the operator asked
 * for it by name.
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
 * `POST /api/plugins/{id}/reload` — stop, re-read the manifest, start again.
 *
 * This is what makes a saved configuration take effect: the kernel reads
 * configuration at spawn / bring-up time, and for a connector the whole child
 * environment is built once per bring-up, so a running plugin is running its
 * old configuration in full — not partly. The response is the plugin's detail
 * *after* the attempt, which is the only thing that says what happened; see
 * [`reloadOutcome`] for why the HTTP status alone cannot.
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

/**
 * The four property types `plugin_host::template_input` accepts. `enum` is not
 * one of them: the kernel allows `enum` only on `type: "string"`, so it is a
 * *constraint on a string field*, carried by [`PluginConfigField.options`],
 * rather than a fifth type.
 */
export type PluginConfigFieldKind = 'string' | 'integer' | 'number' | 'boolean';

export type PluginConfigField = Readonly<{
  /** The key as declared. Also the row's label: it is the name the manifest, the
   *  API and any plugin documentation all use, and humanising it would invent a
   *  second name for the thing the operator has to match. */
  key: string;
  kind: PluginConfigFieldKind;
  /** Non-empty ⇒ the field is a choice, and these are the choices. */
  options: readonly string[];
  description: string | null;
  /**
   * The manifest's default, for display only.
   *
   * It is a **placeholder**, never a value: it must not be pre-filled into a
   * control and must never enter a payload (§2.2.4 / §2.2.5). See
   * [`configDraftFrom`] for the one kind of control that cannot show a
   * placeholder and what it does instead.
   */
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
 * The controls a `config_schema` asks for, in the order it declares them.
 *
 * Reads the kernel's subset and **nothing wider**: root `type: "object"` with a
 * `properties` map, and per property a `type` from the four above plus optional
 * `enum` / `default` / `description`. A property the subset does not cover is
 * dropped rather than guessed at, because a control the kernel would refuse to
 * accept a value for is worse than no control: the operator fills it in and the
 * write 400s on a key they cannot fix.
 *
 * A schema that is not the subset at all yields `[]`, which the caller renders
 * as "nothing to configure here" — the same thing `has_config: false` means.
 * That is deliberate: this decoder never throws, because a screen is not the
 * place to discover that a manifest is malformed, and the kernel already
 * fail-closes on such a manifest at validation time.
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

/**
 * The stored document, or `null` when the row does not hold one.
 *
 * `null` is the corrupt row the kernel refuses to merge into (409
 * `plugin_config_corrupt`), and it is a distinct answer from `{}`: an empty
 * object is a plugin nobody has configured, while this is a row whose contents
 * the operator is about to replace. The difference decides whether a Save may
 * be an ordinary patch at all.
 */
export function storedConfigOf(userConfig: unknown): Readonly<Record<string, unknown>> | null {
  return isPlainObject(userConfig) ? userConfig : null;
}

/** What each control holds. `null` is "no value" — empty field, cleared choice. */
export type PluginConfigDraft = Readonly<Record<string, PluginConfigValue | null>>;

/**
 * What the controls start at, given the stored document.
 *
 * The baseline is `user_config` — **not** `effective_config`. A control seeded
 * from the merged object could not tell "the operator chose `dark`" from "the
 * manifest defaults to `dark`", and [`configPatchFrom`] diffs against exactly
 * this map, so seeding from the merge would post every default back on the
 * first edit of any single key.
 *
 * One kind of control is unavoidably different: a switch has no third position
 * and no placeholder, so a boolean with no stored value starts at its default
 * (then `false`) and is *shown* as such. That is not a value being pre-filled —
 * the diff base is this same displayed state, so a switch nobody touched
 * contributes nothing to a patch, and a switch flipped to the value it already
 * showed contributes nothing either.
 *
 * A stored value whose type does not match the declaration cannot be rendered
 * by the control the declaration asks for, so the field starts empty and the
 * stored value is left alone unless the operator edits that field. Leaving it
 * is the conservative half: the alternative is a Save that silently rewrites a
 * key the operator never looked at.
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
 * The patch body for a Save: the keys whose control moved, and only those.
 *
 * This is #1284 §2.2.5 in one function, and the three properties it owes:
 *
 *   * **a key nobody edited is absent** — `base` is what the controls were
 *     seeded with, so equality against it *is* "untouched";
 *   * **a cleared key is `null`**, which is how the kernel deletes it, and it
 *     only appears when there was something stored to delete;
 *   * **a manifest default is never written.** A default reaches a control only
 *     as a placeholder or as a switch's starting position, and both are part of
 *     `base`, so neither can be mistaken for an edit. Writing defaults back
 *     would freeze today's manifest into the row forever — a later manifest
 *     could change a default and it would never again apply to any plugin
 *     anyone had ever opened this screen for.
 *
 * Keys outside `fields` are never emitted, so residue left by an older
 * manifest — which the kernel deliberately keeps in the row — cannot be
 * disturbed by a screen that does not show it.
 *
 * ## A switch moved *onto* its default deletes the key
 *
 * The third property above has a hole a switch falls straight through, and the
 * hole is only reachable when a value is already stored. `verbose` defaults to
 * `true`, the row stores `false`, the operator flips it back: the control now
 * shows `true`, it differs from the seed, and the literal rule would post
 * `{"verbose": true}` — which is today's manifest default, materialized into
 * the row forever. Every other control has an out (clear the field, clear the
 * choice, and the key is deleted); a switch has two positions and can send
 * neither `null` nor "unset", so on this one control the literal rule and
 * §2.2.4 cannot both hold.
 *
 * So for a boolean, **the value that equals the declared default means
 * "inherit"**, and the patch says `null` — the kernel deletes the key and the
 * manifest default applies again, which is exactly what the switch is showing.
 * The alternative considered was a second, per-row clear affordance beside the
 * switch; it was rejected because the pane's appearance is signed off, a switch
 * that already sits on the default has no visible difference between "stored"
 * and "inherited", and an extra control would ask the operator to distinguish
 * two states that render identically and behave identically until the manifest
 * changes.
 *
 * Two consequences, both intended. A boolean pinned explicitly to the same
 * value as its default is not expressible — that is §2.2.4's whole point, not a
 * loss. And a stored value that *already* equals the default is left alone,
 * because the seed equals it and nothing here fires; it is indistinguishable on
 * screen from the inherited one, and rewriting a row nobody edited is the thing
 * this function exists not to do.
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

/**
 * A kernel refusal, reduced to the two fields every branch of #1284's tables is
 * keyed on: the machine-readable `code` and the sentence the kernel wrote.
 *
 * The transport's own failures get a code too (there is no HTTP body to read
 * one from), so callers of [`reloadOutcome`] and [`configWriteError`] never
 * have to handle "a failure with no code" as a separate shape.
 */
export type PluginApiFailure = Readonly<{ code: string; message: string }>;

/**
 * What one Save did. The refusal is carried **unclassified**: the wording lives
 * in [`configWriteError`], so the caller that performed the request does not
 * hold a second copy of that table.
 */
export type PluginConfigSaveResult =
  | Readonly<{ ok: true }>
  | Readonly<{ ok: false; failure: PluginApiFailure }>;

/**
 * The three facts #1284 §2.4 is written in terms of, read **after** a restart
 * attempt: the refusal (if any), and the plugin's own state and `last_error`.
 *
 * A caller that returned a finished sentence instead would be the second
 * implementation of the §2.4 table — and the one the screen actually shows.
 * [`reloadOutcome`] is the only place that table exists.
 */
export type PluginRestartFacts = Readonly<{
  failure: PluginApiFailure | null;
  state: PluginState;
  lastError?: string;
}>;

/** What one Apply & restart did: it either never got past the write, or it
 *  restarted and left the plugin somewhere. */
export type PluginConfigApplyResult =
  | Readonly<{ saved: false; failure: PluginApiFailure }>
  | Readonly<{ saved: true; restart: PluginRestartFacts }>;

export type PluginConfigWriteError = Readonly<{
  /** The sentence to show. The kernel's own wording wherever the kernel wrote a
   *  usable one — its refusals name the key and the way out, and paraphrasing
   *  them here would produce a second, weaker copy that drifts. */
  message: string;
  /** The declared key the kernel's message named, when it named one this form
   *  actually renders. `null` puts the message on the pane instead. */
  fieldKey: string | null;
  /** Whether `?reset=true` is the kernel's stated exit from this refusal, i.e.
   *  whether the pane should offer the destructive action by name. */
  offersReset: boolean;
}>;

/**
 * `config.<key>: <reason>` — the shape every schema violation from
 * `template_input`'s validator has, since the route passes `"config"` as its
 * root path.
 *
 * Only a key this form renders is accepted as a field match. An undeclared key
 * (`{"ghost": null}` against a schema without `ghost`) is reported by the
 * kernel in the same shape, and it has no control to land on — it belongs on
 * the pane, where "this build is asking for a key the plugin no longer
 * declares" is readable.
 */
function fieldViolationOf(
  message: string,
  fields: readonly PluginConfigField[],
): Readonly<{ key: string; reason: string }> | null {
  const match = /^config\.([^\s:]+):\s*(.+)$/s.exec(message);
  if (match === null) return null;
  const [, key, reason] = match;
  if (key === undefined || reason === undefined) return null;
  return fields.some((field) => field.key === key) ? { key, reason } : null;
}

/**
 * A rejected `PATCH /config`, as something an operator can act on.
 *
 * The kernel's four refusals and what each one leaves to do (the branch table
 * on `routes/plugins.rs::patch_plugin_config`):
 *
 *   * **400** — the patch violates the schema. The message carries the field
 *     path, so it goes *on the field*, which is the difference between "your
 *     configuration is wrong somewhere" and a red control with a reason.
 *   * **409 `plugin_busy`** — the lock is held. Nothing was written; the whole
 *     remedy is to try again, and the message has to say the first half or the
 *     operator will assume a half-applied save.
 *   * **409 `plugin_manifest_unloaded`** — no schema is loaded to judge
 *     against. The kernel's sentence names the two possible fixes (reload, or
 *     repair `manifest.json`) and is better than anything restated here.
 *   * **409 `plugin_config_corrupt`** — the stored document is not an object.
 *     The exit exists and is a single request, so this comes with an offer
 *     rather than only a sentence.
 *   * **400 `plugin_config_too_large`** — the row is over the total cap because
 *     of residue from keys older manifests declared. The kernel's own sentence
 *     ends "resend this request with `?reset=true`", and it is the *only* way
 *     out: no ordinary patch can shrink residue this form does not render. It
 *     therefore takes the same offer. This is why the kernel gives the refusal
 *     its own code — it is a 400 like every schema violation, and the exit it
 *     names cannot be recovered from the message text.
 */
export function configWriteError(
  failure: PluginApiFailure,
  fields: readonly PluginConfigField[],
): PluginConfigWriteError {
  const violation = fieldViolationOf(failure.message, fields);
  if (violation !== null) {
    return { message: violation.reason, fieldKey: violation.key, offersReset: false };
  }
  if (failure.code === 'plugin_busy') {
    return {
      message: 'Another operation is using this plugin right now, so nothing was saved. Try again in a moment.',
      fieldKey: null,
      offersReset: false,
    };
  }
  return {
    message: failure.message,
    fieldKey: null,
    offersReset: failure.code === 'plugin_config_corrupt' || failure.code === 'plugin_config_too_large',
  };
}

/** What a reload attempt actually did, per #1284 §2.4, plus the one ending
 *  §2.4 has no row for — see `unknown` in [`reloadOutcome`]. */
export type PluginReloadOutcomeKind =
  | 'applied' | 'starting' | 'busy' | 'unavailable' | 'stopped' | 'idle' | 'unknown';

export type PluginReloadOutcome = Readonly<{
  kind: PluginReloadOutcomeKind;
  message: string;
  /** `unavailable` is a connector's normal terminal state and `stopped` is a
   *  plugin that did not come back; both are warnings about the plugin, not
   *  errors of the kernel, and the pane paints them accordingly. */
  tone: 'success' | 'warning';
}>;

/**
 * The §2.4 table, as one function over facts read **after** the attempt.
 *
 * The status code is not the verdict, and that is the whole reason this
 * function takes `state` and `lastError` at all:
 *
 *   * a `reload` stops the plugin before it re-reads anything, so a failure is
 *     never "still running the old configuration" — it is a plugin that is
 *     down;
 *   * a connector whose bring-up fails answers non-200 while ending in
 *     `unavailable` + `last_error`, which is that connector's *normal* terminal
 *     state, not a kernel error, and its `last_error` is the only diagnostic
 *     that exists;
 *   * `plugin_busy` is the one failure where nothing at all happened to the
 *     process: the old one is still up, still on its old configuration, and the
 *     configuration the operator saved is safely in the database.
 *
 * Order matters. `plugin_busy` is judged first because it is a statement about
 * the *attempt* — the state that follows it is the state of a plugin nobody
 * touched. `unavailable` is judged before the generic failure because it is
 * more specific than "it did not come up", and it carries the reason verbatim.
 * `unknown` — the one ending §2.4 has no row for, because nothing observed the
 * plugin — comes after both: a transport failure whose read-back still landed a
 * state is a case with evidence, and the evidence wins.
 */
export function reloadOutcome(facts: PluginRestartFacts): PluginReloadOutcome {
  const { failure, state, lastError } = facts;
  if (failure?.code === 'plugin_busy') {
    return {
      kind: 'busy',
      tone: 'warning',
      message: 'Configuration saved. The restart could not run because another operation is using '
        + 'this plugin, so it is still running its previous configuration — try Apply & restart again '
        + 'in a moment.',
      };
  }
  if (state === 'unavailable') {
    /* `last_error` verbatim, appended to one sentence of framing. Verbatim
       because it is the kernel's only account of why a bring-up failed and it
       names things this screen knows nothing about (an upstream host, a
       rejected secret, an exhausted budget); framed because "it is saved" and
       "it is not running" are both facts the operator needs and the kernel's
       sentence states neither. */
    return {
      kind: 'unavailable',
      tone: 'warning',
      message: lastError === undefined
        ? 'Configuration saved. The plugin did not come up with it, and the kernel recorded no reason.'
        : `Configuration saved. The plugin did not come up with it: ${lastError}`,
    };
  }
  if (failure?.code === 'transport_failure' && state === 'unknown') {
    /*
     * The request never left the browser (or came back undecodable), and the
     * read-back that would have settled it failed the same way. §2.4 has three
     * rows and this is none of them: every one of them is a statement about
     * what the *plugin* did, and nothing here observed the plugin at all.
     *
     * Falling through to `stopped` — as this used to — renders the strongest
     * claim on the screen ("the plugin has stopped and did not start with the
     * new configuration") out of the weakest evidence there is. The likeliest
     * truth is the opposite: a request that never arrived did not stop
     * anything, so the plugin is still up on its previous configuration. Both
     * are guesses, so neither is stated.
     */
    return {
      kind: 'unknown',
      tone: 'warning',
      message: 'Configuration saved. The restart request could not be delivered, so this plugin\'s '
        + 'current state is unknown — check the connection and reload this screen to see where it is.',
    };
  }
  if (failure !== null || state === 'crashed') {
    return {
      kind: 'stopped',
      tone: 'warning',
      message: `The plugin has stopped and did not start with the new configuration. ${failure?.message ?? lastError ?? ''}`.trim(),
    };
  }
  if (state === 'spawning' || state === 'installing') {
    return { kind: 'starting', tone: 'success', message: 'Configuration saved. The plugin is starting with it.' };
  }
  if (state === 'running') {
    return { kind: 'applied', tone: 'success', message: 'Configuration saved and the plugin restarted with it.' };
  }
  /* Saved, reloaded, and nothing is running it: a disabled (or never-started)
     plugin re-reads its manifest and stays where it is. Reporting this as
     success would tell the operator their configuration is in force when no
     process holds it. */
  return {
    kind: 'idle',
    tone: 'warning',
    message: 'Configuration saved. This plugin is not running, so enable it to use the new configuration.',
  };
}
