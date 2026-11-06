// Props-only Worker defaults editor. App supplies reads, catalog and save outcomes.
import { Button } from '@astryxdesign/core/Button';
import { Selector } from '@astryxdesign/core/Selector';
import { TextInput } from '@astryxdesign/core/TextInput';
import { useRef } from 'react';
import type { WorkerModelDefaults, WorkerModelProvider } from '../../../../core/api/generated/wire.ts';
import type { ModelCatalog } from '../../../../core/domain/conversation.ts';
import { readWorkerModelDefaults, workerModelDefaultsDraft } from '../../../../core/domain/worker-model-defaults.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { CONTROL_WIDTH, SettingRow, SettingsList, SettingsPane } from './public.tsx';
import styles from './settings.module.css';

export type WorkerDefaultsSaveResult = Readonly<{ ok: true } | { ok: false; message: string }>;
export type WorkerDefaultsProviderProps = Readonly<{
  provider: WorkerModelProvider;
  value: string | undefined;
  catalog: ModelCatalog | undefined;
  catalogError: string | null;
  onRetryCatalog: () => void;
}>;
export type WorkersPaneProps = Readonly<{
  providers: readonly WorkerDefaultsProviderProps[];
  loaded: boolean;
  readRevision: number;
  loadError: string | null;
  onRetryLoad: () => void;
  /** Null means there is no edit capability, including unresolved identity. */
  onSave: ((provider: WorkerModelProvider, value: WorkerModelDefaults | null) => Promise<WorkerDefaultsSaveResult>) | null;
}>;
const LABELS = Object.freeze({ codex: 'Codex', claude: 'Claude' });
const EMPTY = Object.freeze({ model: '', effort: '' });
type Draft = Readonly<{ model: string; effort: string }>;
function draftFrom(raw: string | undefined): Draft {
  const parsed = readWorkerModelDefaults(raw);
  return parsed.ok ? { model: parsed.value?.model ?? '', effort: parsed.value?.effort ?? '' } : EMPTY;
}
function sameStored(raw: string | undefined, value: WorkerModelDefaults | null): boolean {
  const parsed = readWorkerModelDefaults(raw);
  return parsed.ok && (parsed.value === null || value === null ? parsed.value === value
    : parsed.value.model === value.model && parsed.value.effort === value.effort);
}
type SaveState = Readonly<{ phase: 'idle' }
  | { phase: 'pending' | 'saved'; generation: number; value: WorkerModelDefaults | null }
  | { phase: 'failed'; message: string }>;

function ProviderDefaults({ provider, value, catalog, catalogError, onRetryCatalog, loaded, readRevision, onSave, loadError }:
  WorkerDefaultsProviderProps & Omit<WorkersPaneProps, 'providers' | 'onRetryLoad'>) {
  const [seed, setSeed] = useState({ value, readRevision, loaded });
  const [draft, setDraft] = useState(() => draftFrom(value));
  const [dirty, setDirty] = useState(false);
  const [changedElsewhere, setChangedElsewhere] = useState(false);
  const [save, setSave] = useState<SaveState>({ phase: 'idle' });
  const generation = useRef(0);
  const busy = useRef(false);
  const latestRead = useRef({ value, readRevision });
  latestRead.current = { value, readRevision };
  if (loaded && (!seed.loaded || seed.value !== value || seed.readRevision !== readRevision)) {
    setSeed({ value, readRevision, loaded });
    if (!dirty && !busy.current && save.phase === 'idle') {
      setDraft(draftFrom(value));
      setSave({ phase: 'idle' });
      setChangedElsewhere(false);
    } else {
      const matchesWrite = (save.phase === 'pending' || save.phase === 'saved') && sameStored(value, save.value);
      setChangedElsewhere(!matchesWrite && (seed.value !== value || save.phase === 'saved' || changedElsewhere));
      if (save.phase === 'saved') {
        setSave({ phase: 'idle' });
        setDirty(!matchesWrite);
      }
    }
  }
  const edit = (next: Draft) => {
    generation.current += 1;
    setDraft(next);
    setDirty(true);
    if (!busy.current) setSave({ phase: 'idle' });
  };
  const commit = (clear: boolean) => {
    if (busy.current || !loaded || loadError !== null || onSave === null) return;
    const parsed = clear ? { ok: true as const, value: null } : workerModelDefaultsDraft(draft.model, draft.effort);
    if (!parsed.ok) return;
    const submitted = parsed.value;
    const submittedGeneration = generation.current;
    const submittedRevision = readRevision;
    busy.current = true;
    setSave({ phase: 'pending', generation: submittedGeneration, value: submitted });
    void onSave(provider, submitted).then(result => {
      if (!result.ok) { setSave({ phase: 'failed', message: result.message }); return; }
      const current = latestRead.current;
      if (current.readRevision !== submittedRevision && !sameStored(current.value, submitted)) {
        setSave({ phase: 'idle' });
        setChangedElsewhere(true);
        setDirty(true);
      } else if (generation.current === submittedGeneration) {
        if (clear) setDraft(EMPTY);
        setDirty(false);
        setChangedElsewhere(false);
        setSave({ phase: 'saved', generation: submittedGeneration, value: submitted });
      } else setSave({ phase: 'idle' });
    }).finally(() => { busy.current = false; });
  };
  const label = LABELS[provider];
  const stored = readWorkerModelDefaults(value);
  const validation = workerModelDefaultsDraft(draft.model, draft.effort);
  const disabled = !loaded || loadError !== null || onSave === null;
  const selectedModel = catalog?.models.find(entry => entry.model === (draft.model || catalog.default.model));
  const efforts = (draft.model === '' ? catalog?.default.supported_reasoning_efforts : null)
    ?? selectedModel?.supported_reasoning_efforts ?? [];
  return <SettingRow title={label} description={<span>Blank fields inherit provider defaults; these are requested overrides, not the model reported by a running Worker.</span>}
    control={<div className={styles.durationControl} role="group" aria-label={`${label} defaults`}>
      {!stored.ok && <><p role="alert" className={styles.error}>Stored {label} defaults are invalid. {stored.error} Clear defaults to repair them.</p><code>{value}</code></>}
      <TextInput label={`${label} model`} value={draft.model} width={CONTROL_WIDTH} isDisabled={disabled}
        description="Choose from the catalog below or type a model slug."
        onChange={model => edit({ ...draft, model })} />
      {catalog !== undefined && catalog.models.length > 0 && <Selector label={`${label} model catalog`} width={CONTROL_WIDTH}
        value={catalog.models.some(entry => entry.model === draft.model) ? draft.model : ''}
        options={[{ value: '', label: 'Choose a model…' }, ...catalog.models.map(entry => ({ value: entry.model, label: entry.display_name }))]}
        isDisabled={disabled} onChange={model => { if (model !== '') edit({ ...draft, model }); }} />}
      <TextInput label={`${label} effort`} value={draft.effort} width={CONTROL_WIDTH} isDisabled={disabled}
        description="Choose a suggested effort below or type an effort slug."
        onChange={effort => edit({ ...draft, effort })} />
      {efforts.length > 0 && <Selector label={`${label} effort catalog`} width={CONTROL_WIDTH}
        value={efforts.some(entry => entry.reasoning_effort === draft.effort) ? draft.effort : ''}
        options={[{ value: '', label: 'Choose an effort…' }, ...efforts.map(entry => ({ value: entry.reasoning_effort, label: entry.reasoning_effort }))]}
        isDisabled={disabled} onChange={effort => { if (effort !== '') edit({ ...draft, effort }); }} />}
      {catalogError !== null ? <ErrorBox message={catalogError} onRetry={onRetryCatalog} />
        : catalog === undefined ? <span role="status">Loading catalog; manual configuration is available.</span>
          : catalog.source === 'unavailable' && <span role="status">Catalog unavailable; manual configuration is available.</span>}
      {!validation.ok && <p role="alert" className={styles.error}>{validation.error}</p>}
      <div className={styles.actions}>
        <Button label={`Save ${label} defaults`} isDisabled={disabled || busy.current || !validation.ok || (!stored.ok && !dirty)}
          onClick={() => commit(false)}>Save</Button>
        <Button label={`Clear ${label} defaults`} variant="secondary" isDisabled={disabled || busy.current}
          onClick={() => commit(true)}>Clear defaults</Button>
      </div>
      {save.phase === 'failed' && <p role="alert" className={styles.error}>{save.message} Your draft is retained. Save or clear defaults to try again.</p>}
      <span role="status">{save.phase === 'pending' ? 'Saving defaults…' : save.phase === 'saved' ? 'Saved.' : dirty ? 'Your edit is not saved.' : ''}</span>
      {save.phase === 'pending' && generation.current !== save.generation && <span role="status">Your new edit is not saved. Save it after this request finishes.</span>}
      {changedElsewhere && <span role="status">Changed elsewhere. Your draft is retained; review it before saving.</span>}
    </div>} />;
}

export function WorkersPane(props: WorkersPaneProps) {
  return <SettingsPane category="workers" title="Workers"
    lede="Defaults affect only Workers created afterwards; existing Workers keep their original selection, and no restart is needed.">
    {props.loadError !== null && <ErrorBox message={props.loadError} onRetry={props.onRetryLoad} />}
    {props.onSave === null && <p role="status">Only the deployment owner can edit Worker defaults.</p>}
    {!props.loaded ? props.loadError === null && props.onSave !== null && <p role="status">Loading settings…</p>
      : <SettingsList>{props.providers.map(provider => <ProviderDefaults key={provider.provider} {...props} {...provider} />)}</SettingsList>}
  </SettingsPane>;
}
