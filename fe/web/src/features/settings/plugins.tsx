// Settings › Plugins — the installed list, and the one write it offers.
//
// Presentational and props-driven like every other surface in this domain: the
// list, both error strings and the in-flight id arrive as props, and the toggle
// leaves through `onSetEnabled`.
//
// ## Enable is a switch, not a pair of buttons
//
// Enabled is a *state of the plugin*, not an action taken on it, and a switch
// is the control that says so. It also removes the read the button pair forces
// — "the button says Disable, so the plugin is… enabled?" — which is one
// inversion too many on a screen the reader visits once a month.
//
// ## Why `state` and `enabled` are both on the row
//
// They answer different questions and they disagree constantly, which is the
// whole reason this screen exists. `enabled` is what the operator asked for and
// what the kernel persisted; `state` is what the supervisor achieved. A plugin
// that is enabled and `crashed`, or enabled and `unavailable` (a connector
// whose upstream is unreachable — the normal terminal state for one, and
// nothing will retry it), is the case the reader came here to see. Showing only
// the switch would hide it; showing only the state would make the switch's
// position unexplainable.

import { Badge as AstryxBadge } from '@astryxdesign/core/Badge';
import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { Switch as AstryxSwitch } from '@astryxdesign/core/Switch';
import { Text as AstryxText } from '@astryxdesign/core/Text';

import type { PluginListItem, PluginState } from '../../../../core/domain/plugins.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { SettingRow, SettingsList, SettingsPane } from './public.tsx';
import styles from './settings.module.css';

export type PluginsPaneProps = Readonly<{
  /** `undefined` means "still loading" — never render an empty list for it. */
  plugins: readonly PluginListItem[] | undefined;
  loadError: string | null;
  onRetryLoad: () => void;
  /** The plugins a lifecycle write is in flight for. A set, not one id: two
   *  switches can be in flight at once, and one spinner cannot describe both. */
  pendingIds: ReadonlySet<string>;
  /** The last failure **per plugin**. One shared string put one plugin's error
   *  under another plugin's name as soon as two writes overlapped. */
  errors: ReadonlyMap<string, string>;
  onSetEnabled: (id: string, enabled: boolean) => void;
  /**
   * Walk into a plugin's configuration.
   *
   * Offered **only** where `has_config` is true (#1284 §2.5). "This plugin has
   * nothing to configure" and "the configuration screen is not built" have to
   * be two different things on screen, and a Configure button that opens an
   * empty pane is how they became one thing in the first place. The bit is on
   * the list row precisely so this is decidable without a per-row detail
   * fetch — the list carries no manifest, and it never will.
   */
  onOpenConfig: (id: string) => void;
}>;

/**
 * Runtime state → badge tone.
 *
 * `unavailable` is **warning, not error**: it is a connector's normal terminal
 * state, and painting it red would say the kernel is broken when the truth is
 * that an upstream did not answer. `crashed` is the error tone, because
 * something that was meant to be running is not.
 */
function stateVariant(state: PluginState): 'success' | 'warning' | 'error' | 'info' | 'neutral' {
  switch (state) {
    case 'running': return 'success';
    case 'crashed': return 'error';
    case 'unavailable': return 'warning';
    case 'spawning': case 'installing': return 'info';
    default: return 'neutral';
  }
}

export function PluginsPane({
  plugins, loadError, onRetryLoad, pendingIds, errors, onSetEnabled, onOpenConfig,
}: PluginsPaneProps) {
  return (
    <SettingsPane
      title="Plugins"
      lede="What the workspace can do beyond its own kernel. Disabling one keeps its configuration; nothing it created is removed."
    >
      {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
      {plugins === undefined
        ? loadError === null && <AstryxText as="p" color="secondary">Loading plugins…</AstryxText>
        : plugins.length === 0
          ? <AstryxText as="p" color="secondary">No plugins installed.</AstryxText>
          : (
            <SettingsList>
              {plugins.map((plugin) => (
                <SettingRow
                  key={plugin.id}
                  title={plugin.manifest_name}
                  description={(
                    <span className={styles.pluginMeta}>
                      <span className={styles.pluginId}>{plugin.id} · {plugin.version}</span>
                      {plugin.manifest_description !== undefined && (
                        <span>{plugin.manifest_description}</span>
                      )}
                      {/* The reason a row is `crashed` or `unavailable`. Inside
                          the row, not hoisted to a banner: it is a property of
                          this plugin, and two failing plugins must not collapse
                          into one message. */}
                      {plugin.last_error !== undefined && (
                        <span className={styles.error} role="alert">{plugin.last_error}</span>
                      )}
                      {/* This plugin's own failed write, under this plugin's
                          own name. */}
                      {errors.get(plugin.id) !== undefined && (
                        <span className={styles.error} role="alert">{errors.get(plugin.id)}</span>
                      )}
                    </span>
                  )}
                  /* The state badge leads the row: it is the column a reader
                     scans down, and it is the fact `enabled` cannot give them
                     — enabled-and-crashed is the case this screen exists for. */
                  startContent={<AstryxBadge variant={stateVariant(plugin.state)} label={plugin.state} />}
                  control={(
                    /*
                     * Two controls on the trailing edge, and the row itself is
                     * not a click target. The row grammar's rule is that a row
                     * is either something you set or somewhere you go — never
                     * both — because a clickable row wrapping a control is two
                     * targets for one intent. Two adjacent, separately named
                     * controls are two intents with one target each, which is
                     * the shape that rule permits; a drill-in *row* here would
                     * have had to take the switch away.
                     */
                    <span className={styles.pluginControls}>
                      {/* Only when the kernel says there is something to
                          configure — see `onOpenConfig`. */}
                      {plugin.has_config && (
                        <AstryxButton
                          /* Visible "Configure", announced "Configure Todo".
                             astryx's own arrangement for this: `children` is
                             what is painted and `label` becomes the accessible
                             name, so a column of buttons all reading the same
                             word is still navigable by name — and the visible
                             word is contained in the spoken one, which is what
                             keeps speech input working. */
                          label={`Configure ${plugin.manifest_name}`}
                          variant="ghost"
                          onClick={() => onOpenConfig(plugin.id)}
                        >
                          Configure
                        </AstryxButton>
                      )}
                      <AstryxSwitch
                        // Named after the plugin: a list of switches all called
                        // "Enabled" is one a screen reader cannot navigate.
                        label={`Enable ${plugin.manifest_name}`}
                        isLabelHidden
                        value={plugin.enabled}
                        isLoading={pendingIds.has(plugin.id)}
                        onChange={(next) => onSetEnabled(plugin.id, next)}
                      />
                    </span>
                  )}
                />
              ))}
            </SettingsList>
          )}
    </SettingsPane>
  );
}
