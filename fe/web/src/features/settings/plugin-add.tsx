// Draft configuration stays in component memory. Check is a diagnostic request,
// never an installation prerequisite; only Add creates a plugin.
import { useEffect, useRef } from 'react';
import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { Selector as AstryxSelector } from '@astryxdesign/core/Selector';
import { TextArea } from '@astryxdesign/core/TextArea';
import { TextInput as AstryxTextInput } from '@astryxdesign/core/TextInput';
import { parseMcpConfig } from '../../../../core/domain/mcp-config.ts';
import { connectorDraftError, type ConnectorCheckResult, type ConnectorInstallDraft } from '../../../../core/domain/plugins.ts';
import { useState } from '../../ui/state/public.ts';
import { CONTROL_WIDTH, SettingRow, SettingsList, SettingsPane } from './public.tsx';
import styles from './settings.module.css';

export type PluginAddPaneProps = Readonly<{
  pending: boolean;
  onBack: () => void;
  onCheckConnector: (draft: ConnectorInstallDraft) => Promise<ConnectorCheckResult>;
  onInstallConnector: (draft: ConnectorInstallDraft) => Promise<string | null>;
  onInstallLocalPath: (path: string) => Promise<string | null>;
  onInstalled: () => void;
}>;

const SOURCE_OPTIONS = Object.freeze([
  Object.freeze({ value: 'connector', label: 'Remote MCP server' }),
  Object.freeze({ value: 'local_path', label: 'Server directory' }),
] as const);
const TOOL_ACCESS_OPTIONS = Object.freeze([
  Object.freeze({ value: 'all', label: 'All tools' }),
  Object.freeze({ value: 'selected', label: 'Selected tools' }),
] as const);

export function PluginAddPane({ pending, onBack, onCheckConnector, onInstallConnector, onInstallLocalPath, onInstalled }: PluginAddPaneProps) {
  const [source, setSource] = useState('connector');
  const [raw, setRaw] = useState('');
  const [selection, setSelection] = useState<string>();
  const [advanced, setAdvanced] = useState(false);
  const [overrides, setOverrides] = useState<Partial<ConnectorInstallDraft>>({});
  const [path, setPath] = useState('');
  const [error, setError] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const [checked, setChecked] = useState<ConnectorCheckResult | null>(null);
  const generation = useRef(0);
  useEffect(() => () => { generation.current += 1; }, []);

  const parsed = parseMcpConfig(raw, selection);
  const choices = parseMcpConfig(raw);
  const draft = parsed.kind === 'ready' ? { ...parsed.draft, ...overrides } : null;
  const problem = parsed.kind === 'invalid' ? parsed.error
    : parsed.kind === 'choose' ? 'Choose one server to add.'
      : draft === null ? 'Paste a configuration.' : connectorDraftError(draft);

  const invalidate = () => {
    generation.current += 1;
    setChecking(false);
    setChecked(null);
    setError(null);
  };
  const editAdvanced = (change: Partial<ConnectorInstallDraft>) => {
    invalidate();
    setOverrides((value) => ({ ...value, ...change }));
  };
  const check = async () => {
    if (problem !== null || draft === null) { setError(problem); return; }
    const token = ++generation.current;
    setChecking(true);
    setChecked(null);
    setError(null);
    let result: ConnectorCheckResult;
    try { result = await onCheckConnector(draft); }
    catch { result = { ok: false, message: 'The connection check could not finish. Try again.' }; }
    if (generation.current !== token) return;
    setChecking(false);
    setChecked(result);
  };
  const submit = async () => {
    const failure = source === 'connector' ? problem : path.trim() === '' ? 'A directory path is required.' : null;
    if (failure !== null) { setError(failure); return; }
    invalidate();
    try {
      const result = source === 'connector' && draft !== null
        ? await onInstallConnector(draft) : await onInstallLocalPath(path);
      if (result === null) onInstalled(); else setError(result);
    } catch { setError('The plugin could not be added. Try again.'); }
  };

  return (
    <SettingsPane title="Add a plugin" lede="Paste your MCP configuration. Nothing runs until you enable the plugin after adding it.">
      <div className={styles.actions}>
        <AstryxButton label="‹ Plugins" variant="ghost" onClick={onBack} />
      </div>
      <SettingsList>
        <SettingRow title="Source" description="Connect a remote MCP server or use a plugin directory on this server." control={(
          <AstryxSelector label="Source" isLabelHidden value={source} options={[...SOURCE_OPTIONS]} width={CONTROL_WIDTH}
            onChange={(value) => { invalidate(); setSource(value); }} />
        )} />
        {source === 'connector' ? <>
          <SettingRow title="MCP configuration" description="JSON with a URL and optional headers. Keep API keys private." control={(
            <TextArea label="MCP configuration" isLabelHidden width={CONTROL_WIDTH} rows={8} value={raw} isDisabled={pending}
              placeholder={'{\n  "url": "https://example.com/mcp"\n}'}
              onChange={(value: string) => { invalidate(); setRaw(value); setSelection(undefined); setOverrides({}); }} />
          )} />
          {choices.kind === 'choose' && <SettingRow title="Server" description="Choose one server from this configuration." control={(
            <AstryxSelector label="Server" isLabelHidden value={selection ?? ''} width={CONTROL_WIDTH}
              options={[{ value: '', label: 'Choose a server' }, ...choices.names.map((name) => ({ value: name, label: name }))]}
              onChange={(value) => { invalidate(); setSelection(value || undefined); setOverrides({}); }} />
          )} />}
          {draft !== null && <SettingRow title={draft.display_name} description="All tools are available by default. Advanced settings can restrict access." control={(
            <AstryxButton label={advanced ? 'Hide advanced settings' : 'Advanced settings'} variant="ghost" onClick={() => setAdvanced(!advanced)} />
          )} />}
          {advanced && draft !== null && <>
            <SettingRow title="Name" control={<AstryxTextInput label="Name" isLabelHidden value={draft.display_name} width={CONTROL_WIDTH} onChange={(display_name) => editAdvanced({ display_name })} />} />
            <SettingRow title="Id" description="A stable identifier generated from the server name and URL." control={<AstryxTextInput label="Id" isLabelHidden value={draft.id} width={CONTROL_WIDTH} onChange={(id) => editAdvanced({ id })} />} />
            <SettingRow title="Tool access" description="All tools includes new tools on the next enable or reload." control={(
              <AstryxSelector label="Tool access" isLabelHidden value={draft.tool_mode} options={[...TOOL_ACCESS_OPTIONS]} width={CONTROL_WIDTH}
                onChange={(value) => editAdvanced({ tool_mode: value === 'selected' ? 'selected' : 'all' })} />
            )} />
            {draft.tool_mode === 'selected' && <SettingRow title="Tools" description="Enter exact tool names separated by commas, spaces or newlines." control={(
              <AstryxTextInput label="Tools" isLabelHidden value={draft.tools} width={CONTROL_WIDTH} onChange={(tools) => editAdvanced({ tools })} />
            )} />}
          </>}
        </> : <SettingRow title="Directory path" description="A directory containing manifest.json on the machine running Neige Calm." control={(
          <AstryxTextInput label="Directory path" isLabelHidden value={path} width={CONTROL_WIDTH} onChange={(value) => { invalidate(); setPath(value); }} />
        )} />}
      </SettingsList>
      {error !== null && <p role="alert">{error}</p>}
      {checked !== null && (checked.ok
        ? <div role="status" aria-label="Connection check"><p>Connection successful · {checked.tools.length} tools discovered. Tool calls have not been tested.</p>
          <details><summary>View tool names</summary><ul>{checked.tools.map((name, index) => <li key={`${name}-${index}`}>{name}</li>)}</ul></details></div>
        : <p role="alert">{checked.message}</p>)}
      <div className={styles.actions}>
        {source === 'connector' && <AstryxButton label={checking ? 'Checking…' : 'Check connection'} variant="secondary" isDisabled={pending || checking} onClick={() => { void check(); }} />}
        <AstryxButton label={pending ? 'Adding…' : 'Add plugin'} isDisabled={pending} onClick={() => { void submit(); }} />
      </div>
    </SettingsPane>
  );
}
