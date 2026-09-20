// Settings › Plugins › one plugin's configuration.
// A patch carries only the keys the operator edited (the kernel applies manifest defaults on read and never stores them), hence an explicit Save.

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
  /** The plugin's own switch position; Apply & restart is offered only when enabled. */
  enabled: boolean;
  /** `undefined` means "still loading" — never render a form for it: an empty form invites a Save that clears keys the reader never saw. */
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

/** A write that never reached the kernel: nothing was saved and nothing was restarted. */
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
  /* A stored document that is not an object: the kernel refuses to merge into it (409 `plugin_config_corrupt`), so Save must be the named destructive one. */
  const corrupt = detail !== undefined && stored === null;
  const base = useMemo(() => configDraftFrom(fields, stored), [fields, stored]);

  /* Seeded by value, not object identity: the detail arrives as a fresh object every render, and re-seeding on identity would wipe what the reader is typing. */
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
    /* Not an unconditional `setPhase(IDLE)`: a successful write's own refetch re-seeds here and would erase its confirmation. Only a field-level error whose key the schema no longer declares is cleared. */
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
      <div className={styles.actions}>
        <AstryxButton label="‹ Plugins" variant="ghost" onClick={onBack} />
      </div>

      {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
      {detail === undefined
        ? loadError === null && <AstryxText as="p" color="secondary">Loading configuration…</AstryxText>
        : (
          <>
            {corrupt && (
              <p className={styles.notice} role="alert">
                {`This plugin's stored configuration is not readable as a set of keys, so it cannot be `}
                {'patched. Saving discards it and keeps only the values you enter here; every other '}
                {'key falls back to the plugin’s own defaults.'}
              </p>
            )}
            {fields.length === 0
              ? (
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

/** The one line a control cannot say for itself. `required` is enforced when the plugin starts, not on the write. */
function hintFor(field: PluginConfigField): string | null {
  const parts: string[] = [];
  if (field.required) parts.push('Required to start');
  const showsDefault = field.kind === 'boolean' || field.options.length > 0;
  if (showsDefault && field.default !== null) parts.push(`defaults to ${String(field.default)}`);
  return parts.length === 0 ? null : parts.join(' · ');
}

/** One control per declared type. Empty means unset, which is what lets a manifest default keep applying; hence `hasClear`. */
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
      /* Cleared is `null`, never `''`: the kernel deletes a key for `null` and would store an empty string as a value. */
      onChange={(next) => onChange(next === '' ? null : next)}
      width={CONTROL_WIDTH}
    />
  );
}
