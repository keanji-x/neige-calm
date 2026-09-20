// Settings › Plugins — the installed list, and the one write it offers.
// The state chip is keyed on `state`, not `enabled`: nothing enforces that `enabled === false` never coexists with `crashed`.

import { Badge as AstryxBadge } from '@astryxdesign/core/Badge';
import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { IconButton as AstryxIconButton } from '@astryxdesign/core/IconButton';
import { Switch as AstryxSwitch } from '@astryxdesign/core/Switch';
import { Text as AstryxText } from '@astryxdesign/core/Text';
import type { ReactNode } from 'react';

import type { PluginListItem, PluginState } from '../../../../core/domain/plugins.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { SettingRow, SettingsList, SettingsPane } from './public.tsx';
import styles from './settings.module.css';

export type PluginsPaneProps = Readonly<{
  /** `undefined` means "still loading" — never render an empty list for it. */
  plugins: readonly PluginListItem[] | undefined;
  loadError: string | null;
  onRetryLoad: () => void;
  /** The plugins a lifecycle write is in flight for. A set: two switches can be in flight at once. */
  pendingIds: ReadonlySet<string>;
  /** The last failure **per plugin**, never one shared string. */
  errors: ReadonlyMap<string, string>;
  onSetEnabled: (id: string, enabled: boolean) => void;
  /** The plugins whose last enable/disable succeeded — the rows that get the effect-boundary line. */
  effectBoundaryIds: ReadonlySet<string>;
  /** Walk into a plugin's configuration. Offered only where `has_config` is true. */
  onOpenConfig: (id: string) => void;
  /** Walk into the install form. */
  onAdd: () => void;
  /** Remove a plugin — row, token, kv, overlays, and a connector's stored credential. The confirmation is this pane's: the caller is handed an id only once the operator confirmed. */
  onUninstall: (id: string) => void;
}>;

/** Runtime state → badge tone. `unavailable` is warning, not error: a connector's normal terminal state. `installed` is in-progress: the kernel's fallback for "enabled, supervisor has no entry yet". */
function stateVariant(state: PluginState): 'success' | 'warning' | 'error' | 'info' | 'neutral' {
  switch (state) {
    case 'running': return 'success';
    case 'crashed': return 'error';
    case 'unavailable': return 'warning';
    case 'spawning': case 'installing': case 'installed': return 'info';
    default: return 'neutral';
  }
}

/** The switch's annotation, or nothing at all: `disabled` alone gets no chip. */
function stateBadge(state: PluginState): ReactNode {
  if (state === 'disabled') return null;
  return (
    <AstryxBadge
      className={styles.pluginStateChip}
      variant={stateVariant(state)}
      label={state}
    />
  );
}

/** What a successful enable or disable did not reach. Says nothing about tools: the list row cannot tell whether a plugin contributes any. */
const EFFECT_BOUNDARY
  = 'This change doesn’t affect conversations already in progress; it takes effect in a new conversation.';

const ADD_DESCRIPTION = 'A remote MCP server, or a plugin directory on this workspace’s own machine.';

export function PluginsPane({
  plugins, loadError, onRetryLoad, pendingIds, errors, onSetEnabled, onOpenConfig,
  effectBoundaryIds, onAdd, onUninstall,
}: PluginsPaneProps) {
  const [confirming, setConfirming] = useState<string | null>(null);
  return (
    <SettingsPane
      category="plugins"
      title="Plugins"
      lede="What the workspace can do beyond its own kernel. Disabling one keeps its configuration; nothing it created is removed."
    >
      {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
      {plugins === undefined
        ? loadError === null && <AstryxText as="p" color="secondary">Loading plugins…</AstryxText>
        : plugins.length === 0
          ? (
            <>
              <AstryxText as="p" color="secondary">No plugins installed.</AstryxText>
              <SettingsList>
                <SettingRow title="Add a plugin" description={ADD_DESCRIPTION} onOpen={onAdd} />
              </SettingsList>
            </>
          )
          : (
            <SettingsList>
              {plugins.map((plugin) => (
                <SettingRow
                  key={plugin.id}
                  title={plugin.manifest_name}
                  titleSuffix={plugin.version}
                  description={(
                    <span className={styles.pluginMeta}>
                      {plugin.manifest_description !== undefined && (
                        <span>{plugin.manifest_description}</span>
                      )}
                      {plugin.last_error !== undefined && (
                        <span className={styles.error} role="alert">{plugin.last_error}</span>
                      )}
                      {errors.get(plugin.id) !== undefined && (
                        <span className={styles.error} role="alert">{errors.get(plugin.id)}</span>
                      )}
                      {/* Always mounted, text swapped: a live region that arrives in the same mutation as its text is commonly not announced. Silent on a row reporting a failure: `last_error` is server state that arrives after the flag. */}
                      <span
                        className={styles.pluginEffectBoundary}
                        role="status"
                        /* The locator every test of this line uses: astryx renders a hidden `role="status"` inside every `Button`, so the role alone is not unique. */
                        data-nc-effect-boundary=""
                      >
                        {effectBoundaryIds.has(plugin.id) && plugin.last_error === undefined
                          ? EFFECT_BOUNDARY
                          : ''}
                      </span>
                      <span className={styles.pluginId}>{plugin.id}</span>
                      {confirming === plugin.id && (
                        <span className={styles.notice} role="alert">
                          Remove this plugin? Its stored configuration and, for a remote server, its
                          saved key are deleted with it.
                        </span>
                      )}
                    </span>
                  )}
                  control={(
                    <span className={styles.pluginControls} data-nc-plugin-controls="">
                      {/* While a row is confirming, the question replaces the row's other controls. Both buttons name the plugin in their accessible name only. */}
                      {confirming === plugin.id ? (
                        <>
                          <AstryxButton
                            label={`Remove ${plugin.manifest_name}`}
                            variant="destructive"
                            isLoading={pendingIds.has(plugin.id)}
                            onClick={() => {
                              setConfirming(null);
                              onUninstall(plugin.id);
                            }}
                          >
                            Remove
                          </AstryxButton>
                          <AstryxButton
                            label={`Keep ${plugin.manifest_name}`}
                            variant="ghost"
                            onClick={() => setConfirming(null)}
                          >
                            Keep
                          </AstryxButton>
                        </>
                      ) : (
                        <>
                          <AstryxButton
                            label={`Remove ${plugin.manifest_name}`}
                            variant="ghost"
                            isDisabled={pendingIds.has(plugin.id)}
                            onClick={() => setConfirming(plugin.id)}
                          >
                            Remove
                          </AstryxButton>
                      {plugin.has_config && (
                        <AstryxIconButton
                          /* `label` is the whole accessible name (rendered as `aria-label`), so it keeps the plugin's name. */
                          label={`Configure ${plugin.manifest_name}`}
                          variant="ghost"
                          icon={<Icon name="chevron-right" />}
                          onClick={() => onOpenConfig(plugin.id)}
                        />
                      )}
                      {stateBadge(plugin.state)}
                          <AstryxSwitch
                            // Named after the plugin: a list of switches all called "Enabled" cannot be navigated by a screen reader.
                            label={`Enable ${plugin.manifest_name}`}
                            isLabelHidden
                            value={plugin.enabled}
                            isLoading={pendingIds.has(plugin.id)}
                            onChange={(next) => onSetEnabled(plugin.id, next)}
                          />
                        </>
                      )}
                    </span>
                  )}
                />
              ))}
              <SettingRow title="Add a plugin" description={ADD_DESCRIPTION} onOpen={onAdd} />
            </SettingsList>
          )}
    </SettingsPane>
  );
}
