// Settings › Plugins › one plugin's configuration (#1284 §2.5).
//
// Presentational and props-driven like every other settings surface: the
// detail, both error strings and the two writes arrive as props, and this file
// never calls an API.
//
// ## Why this pane has a Save button when no other settings pane does
//
// `README.md`'s INV-SETTINGS-003 — a row commits itself, there is no Save — is
// a rule about *a setting*, and it is right for one: a proxy is one value, and
// asking the reader to press Save for one value is asking them to do the app's
// bookkeeping.
//
// A plugin's configuration is not one value, and #1284 §2.2.5 makes the
// difference structural rather than aesthetic. A patch may carry **only the
// keys the operator edited**: the kernel applies manifest defaults on read and
// never stores them, so a per-row commit of an effective value would write a
// default into the row the moment the reader tabbed through a field — and from
// then on a manifest that changed that default could never reach this plugin
// again. On top of that, nothing here takes effect until the plugin restarts
// (§2.4), so a self-committing row would confirm a save that changes nothing
// observable and leave the operator to discover the restart on their own.
//
// So: one explicit Save for the edited keys, and one explicit Apply & restart
// that is the only thing on the screen that makes them live.
//
// ## The three failure states of a restart are the point, not a detail
//
// A reload stops the plugin *before* re-reading anything, so a failed restart
// is never "carried on with the old configuration" — it is a plugin that is
// down. The verdict therefore cannot come from the HTTP status: it is computed
// by `core/domain/plugins`'s `reloadOutcome` from the plugin's state and
// `last_error` read back after the attempt. This file renders that verdict; it
// does not classify anything itself.

import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { NumberInput as AstryxNumberInput } from '@astryxdesign/core/NumberInput';
import { Selector as AstryxSelector } from '@astryxdesign/core/Selector';
import { Switch as AstryxSwitch } from '@astryxdesign/core/Switch';
import { Text as AstryxText } from '@astryxdesign/core/Text';
import { TextInput as AstryxTextInput } from '@astryxdesign/core/TextInput';
import { useMemo } from 'react';

import {
  configDraftFrom, configFieldsOf, configPatchFrom, configWriteError, reloadOutcome, storedConfigOf,
  type PluginConfigApplyResult, type PluginConfigDraft, type PluginConfigField,
  type PluginConfigSaveResult, type PluginConfigValue, type PluginConfigWriteError,
  type PluginDetail, type PluginReloadOutcome,
} from '../../../../core/domain/plugins.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { CONTROL_WIDTH, SettingRow, SettingsList, SettingsPane } from './public.tsx';
import styles from './settings.module.css';

export type PluginConfigPaneProps = Readonly<{
  pluginId: string;
  pluginName: string;
  /** The plugin's own switch position. A disabled plugin can be configured, but
   *  nothing will run the configuration until it is enabled, and offering
   *  Apply & restart would promise otherwise. */
  enabled: boolean;
  /** `undefined` means "still loading" — never render a form for it: an empty
   *  form invites a Save that clears keys the reader never saw. */
  detail: PluginDetail | undefined;
  loadError: string | null;
  onRetryLoad: () => void;
  onBack: () => void;
  onSave: (
    patch: Readonly<Record<string, PluginConfigValue | null>>,
    options: Readonly<{ reset: boolean }>,
  ) => Promise<PluginConfigSaveResult>;
  onApplyRestart: (
    patch: Readonly<Record<string, PluginConfigValue | null>>,
    options: Readonly<{ reset: boolean }>,
  ) => Promise<PluginConfigApplyResult>;
}>;

type Phase =
  | Readonly<{ phase: 'idle' }>
  | Readonly<{ phase: 'saving' }>
  | Readonly<{ phase: 'restarting' }>
  | Readonly<{ phase: 'saved' }>
  | Readonly<{ phase: 'failed'; error: PluginConfigWriteError }>
  | Readonly<{ phase: 'restarted'; outcome: PluginReloadOutcome }>;

const IDLE: Phase = Object.freeze({ phase: 'idle' });

/** A write that never reached the kernel still has to say something, and it is
 *  not one of §2.4's rows: nothing was saved and nothing was restarted. */
const UNREACHED: PluginConfigWriteError = Object.freeze({
  message: 'The request did not reach the workspace. Check the connection and try again.',
  fieldKey: null,
  offersReset: false,
});

export function PluginConfigPane({
  pluginId, pluginName, enabled, detail, loadError, onRetryLoad, onBack, onSave, onApplyRestart,
}: PluginConfigPaneProps) {
  const schema = detail?.config_schema;
  const fields = useMemo(() => configFieldsOf(schema), [schema]);
  const stored = useMemo(
    () => (detail === undefined ? null : storedConfigOf(detail.user_config)),
    [detail],
  );
  /* A row whose stored document is not an object at all. The kernel refuses to
     merge into it (409 `plugin_config_corrupt`) rather than silently dropping
     whatever it holds, so an ordinary patch cannot repair it and the Save on
     this screen has to be the named, destructive one. Knowing it up front is
     what turns that 409 from a dead end into a labelled button. */
  const corrupt = detail !== undefined && stored === null;
  const base = useMemo(() => configDraftFrom(fields, stored), [fields, stored]);

  /*
   * Seeded **by value**, not by object identity: the detail arrives from a
   * query cache that hands back a fresh object on every render, and re-seeding
   * on identity would wipe out what the reader is typing. A genuine change to
   * the stored document does re-seed — and keeps whichever fields the reader
   * has since edited, because a background refetch must not silently discard
   * an edit in progress either.
   */
  const signature = JSON.stringify(base);
  const [seeded, setSeeded] = useState<Readonly<{ id: string; signature: string; base: PluginConfigDraft }> | null>(null);
  const [draft, setDraft] = useState<PluginConfigDraft>({});
  const [phase, setPhase] = useState<Phase>(IDLE);

  if (detail !== undefined && (seeded === null || seeded.id !== pluginId || seeded.signature !== signature)) {
    const previous = seeded !== null && seeded.id === pluginId ? seeded.base : null;
    setSeeded({ id: pluginId, signature, base });
    setDraft((current) => {
      if (previous === null) return base;
      const next: Record<string, PluginConfigValue | null> = { ...base };
      for (const field of fields) {
        const edited = (current[field.key] ?? null) !== (previous[field.key] ?? null);
        if (edited) next[field.key] = current[field.key] ?? null;
      }
      return next;
    });
    /*
     * ── Why this is not an unconditional `setPhase(IDLE)` ──────────────────
     *
     * It used to be, and that erased the confirmation of every write that
     * worked. `save` / `applyRestart` invalidate this plugin's detail before
     * they resolve, so the successful write's *own* refetch lands a new
     * `user_config`, which changes `signature`, which re-seeds — and the
     * "Saved." / §2.4 sentence the same write had just produced was gone by the
     * next paint. Only the success path could hit it: a refused write changes
     * nothing stored, so the signature holds and the error survived. Worse, a
     * re-seed arriving mid-write cleared `saving` / `restarting`, which
     * re-enabled the buttons under a request still in flight.
     *
     * What a new stored document actually invalidates is narrower than "every
     * verdict": each phase is a statement about a write that already happened,
     * and it stays true no matter what the row says now. The one exception is
     * structural — a field-level error is rendered *inside a control*, so if
     * the schema no longer declares that key there is nowhere to draw it and it
     * would vanish with no trace either way. That, and only that, is cleared.
     */
    if (phase.phase === 'failed'
      && phase.error.fieldKey !== null
      && !fields.some((field) => field.key === phase.error.fieldKey)) {
      setPhase(IDLE);
    }
  }

  const commitBase = seeded !== null && seeded.id === pluginId ? seeded.base : base;
  const patch = configPatchFrom(fields, commitBase, draft);
  const editedKeys = Object.keys(patch);
  const busy = phase.phase === 'saving' || phase.phase === 'restarting';

  const settle = (result: Phase) => { setPhase(result); };

  const save = (reset: boolean) => {
    setPhase({ phase: 'saving' });
    void onSave(patch, { reset })
      .then((result) => {
        settle(result.ok
          ? { phase: 'saved' }
          : { phase: 'failed', error: configWriteError(result.failure, fields) });
      })
      .catch(() => { settle({ phase: 'failed', error: UNREACHED }); });
  };

  const applyRestart = (reset: boolean) => {
    setPhase({ phase: 'restarting' });
    void onApplyRestart(patch, { reset })
      .then((result) => {
        settle(result.saved
          ? { phase: 'restarted', outcome: reloadOutcome(result.restart) }
          : { phase: 'failed', error: configWriteError(result.failure, fields) });
      })
      .catch(() => { settle({ phase: 'failed', error: UNREACHED }); });
  };

  const fieldError = phase.phase === 'failed' ? phase.error : null;
  const paneError = fieldError !== null && fieldError.fieldKey === null ? fieldError : null;

  return (
    <SettingsPane
      title={`${pluginName} configuration`}
      lede="What this plugin runs with. Saving stores the values; the plugin keeps running its previous configuration until it restarts."
    >
      {/* A ghost button above the title, never a filled one beside it: a filled
          button beside a title reads as an action on the thing rather than as
          the way back out of it (README, "Going back from a second level"). */}
      <div className={styles.actions}>
        <AstryxButton label="‹ Plugins" variant="ghost" onClick={onBack} />
      </div>

      {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
      {detail === undefined
        ? loadError === null && <AstryxText as="p" color="secondary">Loading configuration…</AstryxText>
        : (
          <>
            {corrupt && (
              /* Not "replaces it with the values below": the Save sends a
                 patch, and a patch carries only what was edited (§2.2.5). The
                 controls below start empty, and a switch showing `true` because
                 that is the manifest's default is showing an inherited value,
                 not a stored one — none of that reaches the payload. So the
                 honest sentence is that the row is discarded and only what the
                 operator fills in here survives it. */
              <p className={styles.notice} role="alert">
                {`This plugin's stored configuration is not readable as a set of keys, so it cannot be `}
                {'patched. Saving discards it and keeps only the values you enter here; every other '}
                {'key falls back to the plugin’s own defaults.'}
              </p>
            )}
            {fields.length === 0
              ? (
                /* Reached only through a row that said it had configuration, so
                   this is not "nothing to configure" — that row is not offered
                   at all (§2.5). It is a schema the kernel published and this
                   build cannot render, which is a different sentence. */
                <AstryxText as="p" color="secondary">
                  This plugin publishes a configuration schema this build cannot render.
                </AstryxText>
              )
              : (
                <SettingsList>
                  {fields.map((field) => (
                    <SettingRow
                      key={field.key}
                      title={field.key}
                      description={(
                        <span className={styles.pluginMeta}>
                          {field.description !== null && <span>{field.description}</span>}
                          {hintFor(field) !== null && <span className={styles.pluginId}>{hintFor(field)}</span>}
                          {fieldError?.fieldKey === field.key && (
                            <span className={styles.error} role="alert">{fieldError.message}</span>
                          )}
                        </span>
                      )}
                      control={control(field, draft[field.key] ?? null, (value) => {
                        setDraft({ ...draft, [field.key]: value });
                        /* A verdict about the value that was sent is not a
                           verdict about the one being typed. */
                        if (phase.phase !== 'saving' && phase.phase !== 'restarting') setPhase(IDLE);
                      })}
                    />
                  ))}
                </SettingsList>
              )}

            {paneError !== null && (
              <p className={styles.error} role="alert">{paneError.message}</p>
            )}

            <div className={styles.actions}>
              <AstryxButton
                label={corrupt ? 'Replace stored configuration' : 'Save'}
                variant="secondary"
                isLoading={phase.phase === 'saving'}
                isDisabled={busy || (editedKeys.length === 0 && !corrupt)}
                onClick={() => save(corrupt)}
              />
              {enabled && (
                <AstryxButton
                  label="Apply & restart"
                  variant="primary"
                  isLoading={phase.phase === 'restarting'}
                  isDisabled={busy}
                  onClick={() => applyRestart(corrupt)}
                />
              )}
              {/* `?reset=true` is the kernel's own exit from two of its
                  refusals, and it is destructive, so it is offered by name and
                  never taken implicitly. */}
              {paneError?.offersReset === true && (
                <AstryxButton
                  label="Discard stored configuration and save"
                  variant="destructive"
                  isDisabled={busy}
                  onClick={() => save(true)}
                />
              )}
              {phase.phase === 'saved' && (
                <span className={styles.saved} role="status">
                  {enabled
                    ? 'Saved. Apply & restart to run with it.'
                    : 'Saved. Enable this plugin to run with it.'}
                </span>
              )}
              {phase.phase === 'restarted' && (
                <span
                  className={phase.outcome.tone === 'success' ? styles.saved : styles.notice}
                  role="status"
                >
                  {phase.outcome.message}
                </span>
              )}
            </div>
            {!enabled && (
              <AstryxText as="p" color="secondary">
                This plugin is disabled, so nothing is running its configuration. Enable it on the
                previous screen to start it with these values.
              </AstryxText>
            )}
          </>
        )}
    </SettingsPane>
  );
}

/**
 * The one line a control cannot say for itself.
 *
 * A text or number field shows its default as a placeholder, so repeating it
 * would be the same fact twice. A switch and a choice have nowhere to put one —
 * a switch has two positions and no third — so their default is stated here
 * instead. `required` is worth saying wherever it holds: the kernel deliberately
 * does **not** enforce it on the write (that would make a half-filled first
 * save impossible), it enforces it when the plugin starts, so a missing
 * required key surfaces as a plugin that will not come up rather than as a
 * refused Save.
 */
function hintFor(field: PluginConfigField): string | null {
  const parts: string[] = [];
  if (field.required) parts.push('Required to start');
  const showsDefault = field.kind === 'boolean' || field.options.length > 0;
  if (showsDefault && field.default !== null) parts.push(`defaults to ${String(field.default)}`);
  return parts.length === 0 ? null : parts.join(' · ');
}

/**
 * One control per declared type, per §2.5.
 *
 * The controls all agree on one thing: **empty means unset**, and unset is what
 * lets a manifest default keep applying. `NumberInput` and `Selector` get
 * `hasClear` for exactly that reason — without it the reader can move a value
 * but never take it back, and the only way to return a key to its default
 * would be to know the default and retype it.
 */
function control(
  field: PluginConfigField,
  value: PluginConfigValue | null,
  onChange: (next: PluginConfigValue | null) => void,
) {
  const placeholder = field.default === null ? undefined : String(field.default);
  if (field.options.length > 0) {
    return (
      <AstryxSelector
        label={field.key}
        isLabelHidden
        hasClear
        value={typeof value === 'string' ? value : null}
        options={field.options.map((option) => ({ value: option }))}
        placeholder={placeholder ?? 'Not set'}
        onChange={(next) => onChange(next === null || next === '' ? null : next)}
        width={CONTROL_WIDTH}
      />
    );
  }
  if (field.kind === 'boolean') {
    return (
      <AstryxSwitch
        label={field.key}
        isLabelHidden
        value={value === true}
        onChange={(next) => onChange(next)}
      />
    );
  }
  if (field.kind === 'integer' || field.kind === 'number') {
    return (
      <AstryxNumberInput
        label={field.key}
        isLabelHidden
        hasClear
        isIntegerOnly={field.kind === 'integer'}
        value={typeof value === 'number' ? value : null}
        placeholder={placeholder}
        onChange={(next) => onChange(next)}
        width={CONTROL_WIDTH}
      />
    );
  }
  return (
    <AstryxTextInput
      label={field.key}
      isLabelHidden
      value={typeof value === 'string' ? value : ''}
      placeholder={placeholder}
      /* Cleared is `null`, never `''`: the kernel deletes a key for `null` and
         would store an empty string as a value, and "no value" is what lets the
         manifest default apply again (INV-SETTINGS-001, same rule). */
      onChange={(next) => onChange(next === '' ? null : next)}
      width={CONTROL_WIDTH}
    />
  );
}
