import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { TextInput as AstryxTextInput } from '@astryxdesign/core/TextInput';
import { useRef } from 'react';

import { SETTINGS_FAILURES, SETTINGS_TEXT } from '../../../../core/domain/settings.ts';
import { writeFailureText } from '../../../../core/domain/failure-class.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { CONTROL_WIDTH } from './public.tsx';
import styles from './settings.module.css';

/** Only this setting's value and capabilities; app owns the bag, transport and invalidation. */
export type SettingEditorProps = Readonly<{
  value: string | undefined;
  /** Successful-read counter: even an unchanged server value can supersede a save confirmation. */
  readRevision: number;
  loaded: boolean;
  loadError: string | null;
  onRetryLoad: () => void;
  onSave: (value: string | null) => Promise<void>;
}>;

export type SettingEditorCopy = Readonly<{
  label: string;
  unit: string;
  inputMode: 'numeric' | 'text';
  name: string;
  description?: string;
  resetLabel: string;
  parse: (value: string | undefined) => Readonly<{ ok: true; text: string }> | Readonly<{ ok: false; error: string }>;
}>;

function settingText(parse: SettingEditorCopy['parse'], value: string | undefined): string {
  const parsed = parse(value);
  return parsed.ok ? parsed.text : value ?? '';
}

type SettingSave =
  | Readonly<{ phase: 'idle' }>
  | Readonly<{ phase: 'pending' | 'saved'; value: string }>
  | Readonly<{ phase: 'failed'; value: string; message: string }>;

export function SettingEditor({ value, readRevision, loaded, loadError, onRetryLoad, onSave, copy, inlineActions = false }:
  SettingEditorProps & { copy: SettingEditorCopy; inlineActions?: boolean }) {
  const { label, unit, inputMode, name, description, resetLabel, parse } = copy;
  const [seed, setSeed] = useState({ loaded, value, readRevision });
  const [draft, setDraft] = useState(loaded ? settingText(parse, value) : '');
  const [changedElsewhere, setChangedElsewhere] = useState(false);
  const [validationError, setValidationError] = useState<string | null>(null);
  const [save, setSave] = useState<SettingSave>({ phase: 'idle' });
  // One active write. Editing stays available, but no queued or automatically replayed PUT exists.
  const busy = useRef(false);
  const acknowledged = useRef<string | undefined>(undefined);
  const latestRead = useRef({ value, readRevision });
  latestRead.current = { value, readRevision };
  if (loaded && (!seed.loaded || seed.value !== value || seed.readRevision !== readRevision)) {
    const incoming = settingText(parse, value);
    const clean = !seed.loaded || draft === settingText(parse, seed.value);
    setSeed({ loaded, value, readRevision });
    acknowledged.current = undefined;
    if (clean && !busy.current && save.phase === 'idle') {
      setDraft(incoming);
      setChangedElsewhere(false);
      setSave({ phase: 'idle' });
      setValidationError(null);
    } else {
      setChangedElsewhere(draft !== incoming && !(save.phase === 'pending' && save.value === incoming));
      if (save.phase === 'saved' && save.value !== incoming) setSave({ phase: 'idle' });
    }
  }
  const commit = (reset: boolean, explicit: boolean) => {
    if (busy.current || !loaded) return;
    const parsed = parse(reset ? undefined : draft);
    if (!parsed.ok) { setValidationError(parsed.error); return; }
    const text = parsed.text;
    // Leaving a failed field is not permission to silently retry it.
    if (!reset && !explicit && save.phase === 'failed' && save.value === text) return;
    if (!reset && save.phase !== 'failed' && (text === acknowledged.current
      || (parse(value).ok && text === settingText(parse, value)))) return;
    busy.current = true;
    setDraft(text);
    setChangedElsewhere(false);
    setValidationError(null);
    setSave({ phase: 'pending', value: text });
    const submittedRevision = readRevision;
    void (async () => {
      try {
        await onSave(reset ? null : text);
        const read = latestRead.current;
        if (read.readRevision !== submittedRevision && settingText(parse, read.value) !== text) {
          acknowledged.current = undefined;
          setSave({ phase: 'idle' });
          return;
        }
        acknowledged.current = text;
        setSave({ phase: 'saved', value: text });
      } catch (error: unknown) {
        acknowledged.current = undefined;
        setSave({ phase: 'failed', value: text,
          message: writeFailureText(SETTINGS_FAILURES, SETTINGS_TEXT)(error) ?? SETTINGS_TEXT.unknown });
      } finally { busy.current = false; }
    })();
  };
  const stored = parse(value);
  const parsedDraft = parse(draft);
  const inputError = parsedDraft.ok ? null
    : !stored.ok && draft === settingText(parse, value)
      ? `Stored ${name} is invalid. ${stored.error} Restore default to repair it.`
      : validationError ?? parsedDraft.error;
  return <div className={styles.durationControl}>
    {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
    {!loaded ? loadError === null && <span role="status">Loading {name}…</span> : <>
      <div className={inlineActions ? styles.mailActions : styles.durationControl}>
        <AstryxTextInput label={label} value={draft} width={inlineActions ? 160 : CONTROL_WIDTH}
          ref={(input) => { if (input !== null) input.inputMode = inputMode; }}
          description={description}
          status={inputError === null ? undefined : { type: 'error', message: inputError }}
          onChange={(next) => { setDraft(next); setValidationError(null); }}
          onBlur={() => commit(false, false)}
          onEnter={() => commit(false, true)} />
        <AstryxButton label={resetLabel} variant="secondary" isDisabled={save.phase === 'pending'}
          onPointerDown={(event) => event.preventDefault()} onClick={() => commit(true, true)}>Restore default</AstryxButton>
      </div>
      {save.phase === 'failed' && <p className={styles.error} role="alert">{save.value}{inputMode === 'numeric' ? ` ${unit}` : ''}: {save.message} Press Enter to try again or restore default.</p>}
      <span role="status">{save.phase === 'pending' ? `Saving ${save.value}${inputMode === 'numeric' ? ` ${unit}` : ''}…`
        : save.phase === 'saved' && save.value === draft ? 'Saved.'
          : draft !== settingText(parse, value) ? 'Your edit is not saved.' : ''}</span>
      {save.phase === 'pending' && save.value !== draft && <span role="status">Your new edit is not saved. Press Enter after this save finishes.</span>}
      {changedElsewhere && <span role="status">Changed elsewhere. Your edit is retained; review it before saving.</span>}
    </>}
  </div>;
}
