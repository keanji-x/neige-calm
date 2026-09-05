// Settings › Plugins › Add — the form that installs one (#1480).
//
// Presentational and props-driven like every other surface here: the draft
// lives in this component, the write leaves through `onInstall*`, and the only
// thing this file knows about the kernel is that it answers with a sentence
// when it refuses.
//
// ## Why a second level rather than a row on the list
//
// An install is not a setting. It is several fields that only mean anything
// together — a URL with no credential and a credential with no URL are both
// half a plugin — and it has a moment of commitment: `POST /install` creates a
// row, writes a tree and stores a credential. That is the same shape as the
// plugin *configuration* pane one file over, and it is why both have an
// explicit button while INV-SETTINGS-003 keeps every ordinary settings row
// committing itself.
//
// ## Two sources, and they are not two ways to do one thing
//
// * **Remote MCP server** is the one the operator can complete on their own:
//   they have a URL and a key, and the kernel writes the plugin tree for them.
// * **Server directory** installs a tree that already exists *on the machine
//   the workspace runs on* — the only way to install a plugin that runs code.
//   The path is resolved in the kernel's filesystem, which is why this asks for
//   a path instead of offering a file picker: a picker would read the operator's
//   own computer, which is not where the plugin has to be.
//
// A selector rather than two panes: they answer one question ("where does this
// plugin come from"), the fields below it change with the answer, and the
// second source is one field.
//
// ## The API key is typed once and never read back
//
// The field is a password input, the value is held only until the request goes
// out, and nothing on this screen or in the plugin list can display it
// afterwards — the kernel writes it into a `secrets.json` it keeps `0600` and
// never echoes it in a response. Editing a stored key is therefore not offered
// here: it is not an edit, it is a fresh install, and pretending otherwise
// would need a control that shows what it cannot show.

import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { Selector as AstryxSelector } from '@astryxdesign/core/Selector';
import { Text as AstryxText } from '@astryxdesign/core/Text';
import { TextInput as AstryxTextInput } from '@astryxdesign/core/TextInput';

import {
  EMPTY_CONNECTOR_DRAFT, connectorDraftError,
  type ApiKeyPlacement, type ConnectorInstallDraft,
} from '../../../../core/domain/plugins.ts';
import { useState } from '../../ui/state/public.ts';
import { CONTROL_WIDTH, SettingRow, SettingsList, SettingsPane } from './public.tsx';
import styles from './settings.module.css';

export type PluginAddPaneProps = Readonly<{
  /** True while an install is in flight. */
  pending: boolean;
  onBack: () => void;
  /** Both resolve with the kernel's refusal, or `null` when the plugin was
   *  installed. A rejected promise would take the operator's typing with it. */
  onInstallConnector: (draft: ConnectorInstallDraft) => Promise<string | null>;
  onInstallLocalPath: (path: string) => Promise<string | null>;
  /** Called after an install the kernel accepted, so the caller can leave. */
  onInstalled: () => void;
}>;

type Source = 'connector' | 'local_path';

const SOURCE_OPTIONS = Object.freeze([
  Object.freeze({ value: 'connector', label: 'Remote MCP server' }),
  Object.freeze({ value: 'local_path', label: 'Server directory' }),
] as const);

const PLACEMENT_OPTIONS = Object.freeze([
  Object.freeze({ value: 'bearer', label: 'Authorization: Bearer' }),
  Object.freeze({ value: 'header', label: 'Custom header' }),
] as const);

export function PluginAddPane({
  pending, onBack, onInstallConnector, onInstallLocalPath, onInstalled,
}: PluginAddPaneProps) {
  const [source, setSource] = useState<Source>('connector');
  const [draft, setDraft] = useState<ConnectorInstallDraft>(EMPTY_CONNECTOR_DRAFT);
  const [path, setPath] = useState('');
  /* The kernel's refusal, or the one refusal this form makes on its own. Both
     are cleared by typing: a verdict about the values that were sent is not a
     verdict about the ones being edited. */
  const [error, setError] = useState<string | null>(null);

  const localDraftError = source === 'connector'
    ? connectorDraftError(draft)
    : (path.trim() === '' ? 'A directory path is required.' : null);

  const edit = (next: ConnectorInstallDraft) => {
    setDraft(next);
    setError(null);
  };

  const submit = () => {
    if (localDraftError !== null) {
      setError(localDraftError);
      return;
    }
    setError(null);
    const write = source === 'connector'
      ? onInstallConnector(draft)
      : onInstallLocalPath(path);
    void write.then((failure) => {
      if (failure === null) onInstalled();
      else setError(failure);
    });
  };

  return (
    <SettingsPane
      title="Add a plugin"
      lede="Where this plugin comes from. Nothing runs until you enable it on the previous screen."
    >
      <div className={styles.actions}>
        <AstryxButton label="‹ Plugins" variant="ghost" onClick={onBack} />
      </div>

      <SettingsList>
        <SettingRow
          title="Source"
          description="A remote MCP server the workspace calls, or a plugin directory already on the server."
          control={(
            <AstryxSelector
              label="Source"
              isLabelHidden
              value={source}
              options={[...SOURCE_OPTIONS]}
              onChange={(value) => {
                setSource(value === 'local_path' ? 'local_path' : 'connector');
                setError(null);
              }}
              width={CONTROL_WIDTH}
            />
          )}
        />

        {source === 'connector' ? (
          <>
            <SettingRow
              title="Name"
              description="What the plugin is called in this list."
              control={(
                <AstryxTextInput
                  label="Name"
                  isLabelHidden
                  value={draft.display_name}
                  placeholder="Zhibao"
                  onChange={(value) => edit({ ...draft, display_name: value })}
                  width={CONTROL_WIDTH}
                />
              )}
            />
            <SettingRow
              title="Id"
              /* The kernel owns what a legal id is (`is_valid_plugin_id`), and
                 it owns uniqueness too — a taken id comes back as a 409 with
                 its own sentence. This line says what the id is *for*, which is
                 the part no error message will tell them. */
              description="Stable key for this plugin: lower-case letters, digits, dots and dashes."
              control={(
                <AstryxTextInput
                  label="Id"
                  isLabelHidden
                  value={draft.id}
                  placeholder="com.example.zhibao"
                  onChange={(value) => edit({ ...draft, id: value })}
                  width={CONTROL_WIDTH}
                />
              )}
            />
            <SettingRow
              title="Server URL"
              description="The streamable-HTTP MCP endpoint."
              control={(
                <AstryxTextInput
                  label="Server URL"
                  isLabelHidden
                  value={draft.url}
                  placeholder="https://mcp.example.com/mcp"
                  onChange={(value) => edit({ ...draft, url: value })}
                  width={CONTROL_WIDTH}
                />
              )}
            />
            <SettingRow
              title="API key"
              /* Two facts the operator needs before they paste a credential:
                 where it goes, and that this is their last look at it. */
              description="Stored on the server, never shown again. Leave empty for a server that needs no key."
              control={(
                <AstryxTextInput
                  label="API key"
                  isLabelHidden
                  type="password"
                  value={draft.api_key}
                  onChange={(value) => edit({ ...draft, api_key: value })}
                  width={CONTROL_WIDTH}
                />
              )}
            />
            {draft.api_key.trim() !== '' && (
              <SettingRow
                title="Key placement"
                description="How the key is sent. Most servers take a bearer token."
                control={(
                  <AstryxSelector
                    label="Key placement"
                    isLabelHidden
                    value={draft.placement}
                    options={[...PLACEMENT_OPTIONS]}
                    onChange={(value) => edit({
                      ...draft,
                      placement: (value === 'header' ? 'header' : 'bearer') satisfies ApiKeyPlacement,
                    })}
                    width={CONTROL_WIDTH}
                  />
                )}
              />
            )}
            {draft.api_key.trim() !== '' && draft.placement === 'header' && (
              <SettingRow
                title="Header name"
                description="The key is sent under this header, with no prefix."
                control={(
                  <AstryxTextInput
                    label="Header name"
                    isLabelHidden
                    value={draft.header_name}
                    placeholder="X-API-Key"
                    onChange={(value) => edit({ ...draft, header_name: value })}
                    width={CONTROL_WIDTH}
                  />
                )}
              />
            )}
          </>
        ) : (
          <SettingRow
            title="Directory path"
            description="A directory on the machine running this workspace, containing manifest.json."
            control={(
              <AstryxTextInput
                label="Directory path"
                isLabelHidden
                value={path}
                placeholder="/srv/neige/plugins/todo"
                onChange={(value) => { setPath(value); setError(null); }}
                width={CONTROL_WIDTH}
              />
            )}
          />
        )}
      </SettingsList>

      {error !== null && <p className={styles.error} role="alert">{error}</p>}

      <div className={styles.actions}>
        <AstryxButton
          label="Add plugin"
          variant="primary"
          isLoading={pending}
          isDisabled={pending}
          onClick={submit}
        />
      </div>
      <AstryxText as="p" color="secondary">
        A new plugin is installed switched off. Enable it on the previous screen to start it.
      </AstryxText>
    </SettingsPane>
  );
}
