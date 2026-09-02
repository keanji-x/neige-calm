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
import { Heading as AstryxHeading } from '@astryxdesign/core/Heading';
import { List as AstryxList, ListItem as AstryxListItem } from '@astryxdesign/core/List';
import { Switch as AstryxSwitch } from '@astryxdesign/core/Switch';
import { Text as AstryxText } from '@astryxdesign/core/Text';

import type { PluginListItem, PluginState } from '../../../../core/domain/plugins.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import styles from './settings.module.css';

export type PluginsPaneProps = Readonly<{
  /** `undefined` means "still loading" — never render an empty list for it. */
  plugins: readonly PluginListItem[] | undefined;
  loadError: string | null;
  onRetryLoad: () => void;
  /** The plugin a lifecycle write is in flight for, or `null`. */
  pendingId: string | null;
  /** The last lifecycle write's failure. Sits above the list, not inside a row:
   *  the row it belongs to may have been re-read and replaced by then. */
  actionError: string | null;
  onSetEnabled: (id: string, enabled: boolean) => void;
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
  plugins, loadError, onRetryLoad, pendingId, actionError, onSetEnabled,
}: PluginsPaneProps) {
  return (
    <div className={styles.paneBody}>
      <div className={styles.form}>
        <section className={styles.group} aria-labelledby="nc-settings-plugins">
          {/* The same heading weight the Templates pane uses: both are a pane's
              own title, and General's small grey labels name *cards inside* a
              pane, which is a level down. */}
          <AstryxHeading level={3} id="nc-settings-plugins">Plugins</AstryxHeading>
          <AstryxText as="p" color="secondary">
            What the workspace can do beyond its own kernel. A disabled plugin keeps its
            configuration; nothing it created is removed.
          </AstryxText>

          {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
          {actionError !== null && <p className={styles.error} role="alert">{actionError}</p>}

          {plugins === undefined
            ? loadError === null && <AstryxText as="p" color="secondary">Loading plugins…</AstryxText>
            : plugins.length === 0
              ? <AstryxText as="p" color="secondary">No plugins installed.</AstryxText>
              : (
                <AstryxList hasDividers density="balanced">
                  {plugins.map((plugin) => (
                    <AstryxListItem
                      key={plugin.id}
                      /* Same row grammar as the template list: identity on the
                         left, the row's one control at the end. These rows do
                         **not** drill in, so they carry no `onClick` and no
                         chevron — astryx's list guidance rejects an
                         interactive control inside an interactive row, and a
                         row that is both a switch and a link is two intents on
                         one target. */
                      label={plugin.manifest_name}
                      description={(
                        <span className={styles.pluginMeta}>
                          <span className={styles.pluginId}>{plugin.id} · {plugin.version}</span>
                          {plugin.manifest_description !== undefined && (
                            <span>{plugin.manifest_description}</span>
                          )}
                          {/* The reason a row is `crashed` or `unavailable`.
                              Inside the row, not hoisted to a banner: it is a
                              property of this plugin, and two failing plugins
                              must not collapse into one message. */}
                          {plugin.last_error !== undefined && (
                            <span className={styles.error} role="alert">{plugin.last_error}</span>
                          )}
                        </span>
                      )}
                      startContent={<AstryxBadge variant={stateVariant(plugin.state)} label={plugin.state} />}
                      endContent={(
                        <AstryxSwitch
                          // Named after the plugin: a list of switches all
                          // called "Enabled" is one a screen reader cannot
                          // navigate.
                          label={`Enable ${plugin.manifest_name}`}
                          isLabelHidden
                          value={plugin.enabled}
                          isLoading={pendingId === plugin.id}
                          onChange={(next) => onSetEnabled(plugin.id, next)}
                        />
                      )}
                    />
                  ))}
                </AstryxList>
              )}
        </section>
      </div>
    </div>
  );
}
