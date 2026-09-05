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
// ## The state chip lives beside the switch, and `disabled` has none
//
// `enabled` and `state` answer different questions and they disagree
// constantly, which is the whole reason this screen exists. `enabled` is what
// the operator asked for and what the kernel persisted; `state` is what the
// supervisor achieved. A plugin that is enabled and `crashed`, or enabled and
// `unavailable` (a connector whose upstream is unreachable — the normal
// terminal state for one, and nothing will retry it), is the case the reader
// came here to see. Showing only the switch would hide it; showing only the
// state would make the switch's position unexplainable.
//
// So the chip is *the switch's own annotation* and is set immediately before
// it, rather than being a badge at the head of the row. The row used to state
// the same subject twice, at opposite edges, and the reader had to carry one
// across the other to notice they disagreed — which is precisely the one thing
// the row is for. Beside the switch, "asked for on, came up crashed" is a
// single glance.
//
// ## Why the exception is keyed on `state`, not on `enabled`
//
// `disabled` gets no chip: it is the switch's off position said a second time,
// and the chip's whole grammar is "what `enabled` could not tell you". In
// practice that reads exactly as "off shows only the switch; turn it on and the
// achieved state appears in the same place".
//
// It is deliberately **not** written as `enabled ? chip : null`, even though
// the kernel today makes those two conditions coincide. They coincide by
// emergence, not by construction, and the emergence spans two subsystems:
// `GET /api/plugins` takes `state` from the supervisor's in-memory table
// (`PluginHost::status`) and `enabled` from the plugins row, and only synthesises
// `disabled`/`installed` from `enabled` when the table has **no** entry at all
// (`routes/plugins.rs::list_plugins`). Every writer of that table happens to
// honour the bit today — `disable` stops (removing the entry) before the DB
// write, `spawn` short-circuits with `HostError::Disabled` on a row that is not
// enabled, `reload` re-checks `plug.enabled`, and `install` inserts
// `enabled: false` without spawning — but nothing *enforces* it, and a single
// new writer that skipped the check would make `enabled === false` coexist with
// `crashed`. Keying on `enabled` would then hide a crash from the operator to
// keep the row tidy. Keying on `state` cannot: only the one redundant word is
// ever suppressed, and every other state is visible no matter what `enabled`
// says.

import { Badge as AstryxBadge } from '@astryxdesign/core/Badge';
import { IconButton as AstryxIconButton } from '@astryxdesign/core/IconButton';
import { Switch as AstryxSwitch } from '@astryxdesign/core/Switch';
import { Text as AstryxText } from '@astryxdesign/core/Text';
import type { ReactNode } from 'react';

import type { PluginListItem, PluginState } from '../../../../core/domain/plugins.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
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
   * The plugins whose last enable/disable succeeded — the rows that get the
   * effect-boundary line. See `EFFECT_BOUNDARY` for what the line says and why
   * it says that.
   */
  effectBoundaryIds: ReadonlySet<string>;
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
 *
 * `installed` is the **in-progress** tone, with `spawning` and `installing`.
 * It is the kernel's fallback for "enabled, but the supervisor's table has no
 * entry yet" (`routes/plugins.rs::list_plugins`), which is what a plugin the
 * operator just switched on shows for as long as it takes the supervisor to
 * pick it up. Painting that neutral put a grey chip that reads like a verdict
 * next to a switch that is on — an inconsistency the reader would go looking
 * for and not find. The three states that share this tone share a meaning: on
 * the way to `running`, nothing to do.
 *
 * That leaves the neutral tone to `unknown` alone — a state name this build
 * does not have a word for — which is the only state that really is "no
 * information".
 *
 * The tone names are astryx's; what each one *looks like* here is
 * `settings.module.css`'s `.pluginStateChip`, which makes all five one solid
 * mark so the column reads as one column.
 */
function stateVariant(state: PluginState): 'success' | 'warning' | 'error' | 'info' | 'neutral' {
  switch (state) {
    case 'running': return 'success';
    case 'crashed': return 'error';
    case 'unavailable': return 'warning';
    case 'spawning': case 'installing': case 'installed': return 'info';
    default: return 'neutral';
  }
}

/**
 * The switch's annotation, or nothing at all.
 *
 * `disabled` alone gets no chip — see the header note for why the test is on
 * `state` rather than on `enabled`. Every other state, including the neutral
 * ones, is something the switch cannot say, so it is shown.
 */
function stateBadge(state: PluginState): ReactNode {
  if (state === 'disabled') return null;
  return (
    <AstryxBadge
      /* The chip's own appearance, as scoped token overrides on the badge —
         `settings.module.css` says why it is not a variant and not a wrapper. */
      className={styles.pluginStateChip}
      variant={stateVariant(state)}
      label={state}
    />
  );
}

/**
 * What a successful enable or disable did **not** reach (#1242).
 *
 * ## The sentence is about *this change*, not about tools
 *
 * It would read better as "New tools will appear in a new conversation", and
 * that sentence would be a lie on any plugin that contributes no tools.
 *
 * The list row cannot tell them apart: `pluginListItemSchema` has no
 * `exposes_tools`, and the row carries no manifest. The *write's response*
 * can — `setPluginEnabledOperation` decodes `.loose()`, so the `PluginDetail`
 * that `enable`/`disable` answer with keeps its `manifest`, and
 * `manifest.exposes_tools` is sitting in the argument `usePluginMutations`
 * currently discards. No route, schema or specification change would be needed
 * to read it.
 *
 * It is still not read, and the reason is not cost. Copy derived from it would
 * be true only of whichever write last answered: the row would say "new tools"
 * or not depending on a response, while the list beside it refreshes from a
 * different read, and a row that had not been written this visit would have no
 * answer at all. That is a per-row sentence whose truth depends on history the
 * row does not display. "This change doesn't affect conversations already in
 * progress" needs no such history — it is true of every plugin on every render,
 * because the subject is the write the operator just made and not the plugin's
 * contents — and it buys the same behaviour, which is none. The word *tools*
 * does not appear in the line at all, and the test holds it to that.
 *
 * ## And it is true of both directions
 *
 * Not a note attached to enabling. A conversation that is already running holds
 * the tool list it started with, so a *disable* leaves it holding tools the
 * kernel no longer offers exactly as an enable leaves it missing ones it does.
 * The pane's caller sets the flag on either write's success; nothing here
 * branches on `enabled`.
 *
 * ## Why it is a statement of fact and not a warning
 *
 * The boundary is not a failure and there is nothing for the operator to fix —
 * measured against codex 0.144.1 there is no mechanism that would refresh a
 * running conversation's tool list, so this is simply where the change stops.
 * `.error`'s and `.notice`'s boxes both say "something wants attention"; this
 * line takes the row's own quiet meta tone instead, and sits with the other
 * per-row facts rather than in the trailing controls cluster, which the header
 * note keeps to two controls.
 *
 * Two clauses on a semicolon, which is this pane's own register: its lede is
 * built the same way — "Disabling one keeps its configuration; nothing it
 * created is removed."
 */
const EFFECT_BOUNDARY
  = 'This change doesn’t affect conversations already in progress; it takes effect in a new conversation.';

export function PluginsPane({
  plugins, loadError, onRetryLoad, pendingIds, errors, onSetEnabled, onOpenConfig,
  effectBoundaryIds,
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
                  /* The version belongs to the name, so it sits on the name's
                     line. It used to open the description line — where it was
                     the first of three unrelated fragments, and where a reader
                     looking for "what is this plugin" read a build number
                     first. */
                  titleSuffix={plugin.version}
                  description={(
                    <span className={styles.pluginMeta}>
                      {/* Line two is the manifest's own sentence and nothing
                          else: it is the only line here that answers "what is
                          this", and it now gets to answer it alone. */}
                      {plugin.manifest_description !== undefined && (
                        <span>{plugin.manifest_description}</span>
                      )}
                      {/* The reason a row is `crashed` or `unavailable`. Inside
                          the row, not hoisted to a banner: it is a property of
                          this plugin, and two failing plugins must not collapse
                          into one message. Directly under the sentence it
                          contradicts, and above the identity line, because a
                          failure is read and the id is looked up. */}
                      {plugin.last_error !== undefined && (
                        <span className={styles.error} role="alert">{plugin.last_error}</span>
                      )}
                      {/* This plugin's own failed write, under this plugin's
                          own name. */}
                      {errors.get(plugin.id) !== undefined && (
                        <span className={styles.error} role="alert">{errors.get(plugin.id)}</span>
                      )}
                      {/*
                       * Where this plugin's own successful write stopped — see
                       * `EFFECT_BOUNDARY`. `status`, not `alert`: it follows a
                       * write that worked, and it interrupts nothing.
                       *
                       * ## Always mounted, text swapped
                       *
                       * A live region that arrives in the same mutation as its
                       * text is commonly not announced at all, so the region is
                       * here on every row from first paint and only its text
                       * changes. `NetworkPane` one file over does the same for
                       * the same reason (`public.tsx`'s per-field `status`
                       * region, pinned by its "mounts the live region before it
                       * has anything to say" test); `plugin-config.tsx` uses
                       * the weaker conditional form, which is a precedent worth
                       * not following twice.
                       *
                       * The empty region is removed from the flow by
                       * `.pluginEffectBoundary:empty` rather than by `hidden`
                       * or `display: none` — see the CSS. Hiding it would put
                       * it back outside the accessibility tree until the text
                       * arrives, which is the failure this shape exists to
                       * avoid; leaving it a flex item would add
                       * `.pluginMeta`'s gap under every row in the list. That
                       * it costs nothing is measured, not reasoned:
                       * `plugins-live-region.browser.test.tsx` puts the row's
                       * meta column at the same height with the empty region
                       * present and with it deleted, and one `row-gap` taller
                       * with the rule overridden.
                       *
                       * ## Silent on a row that is reporting a failure
                       *
                       * Two `role="alert"` lines sit above this one and they
                       * are not the same kind of thing.
                       *
                       * The *write* error is disjoint from this line by
                       * construction: `usePluginMutations` clears the boundary
                       * flag in `onMutate` and sets it only in `onSuccess`, so
                       * a row can carry one or the other and never both.
                       *
                       * `last_error` is **server state** and nothing in this
                       * screen's write path clears it. It also arrives *after*
                       * the flag on the ordinary path: enable answers 200 as
                       * soon as the row flips, the supervisor is still bringing
                       * the process up, and a crash comes back on the next
                       * poll. So the flag is already set when the failure
                       * lands, and no set-time check could have caught it —
                       * which is why this is a render-time test. A plugin that
                       * crashed will not take effect in a new conversation
                       * either, and saying it will, directly under the evidence
                       * that it did not start, is the one reading of this line
                       * that is simply false.
                       */}
                      <span
                        className={styles.pluginEffectBoundary}
                        role="status"
                      >
                        {effectBoundaryIds.has(plugin.id) && plugin.last_error === undefined
                          ? EFFECT_BOUNDARY
                          : ''}
                      </span>
                      {/* The id last, and quietest. It is not a description of
                          the plugin — it is the key the operator carries to a
                          manifest, a log line or the CLI, which is a lookup and
                          not a scan. It stays monospaced and selectable for
                          exactly that, and it does not disappear: a row whose
                          display name has drifted from its id is unfindable
                          without it. */}
                      <span className={styles.pluginId}>{plugin.id}</span>
                    </span>
                  )}
                  control={(
                    /*
                     * The trailing edge: the drill-in, the state chip, and the
                     * switch the chip annotates. Two *controls* — the chip is
                     * read, not pressed — and the row itself is not a click
                     * target. The row grammar's rule is that a row is either
                     * something you set or somewhere you go — never both —
                     * because a clickable row wrapping a control is two targets
                     * for one intent. Two adjacent, separately named controls
                     * are two intents with one target each, which is the shape
                     * that rule permits; a drill-in *row* here would have had to
                     * take the switch away.
                     */
                    <span className={styles.pluginControls} data-nc-plugin-controls="">
                      {/* Only when the kernel says there is something to
                          configure — see `onOpenConfig`. */}
                      {plugin.has_config && (
                        <AstryxIconButton
                          /*
                           * A glyph, not the word "Configure": every row that
                           * offers configuration spends its trailing edge on
                           * the same word, so the column repeats one verb down
                           * its length and says each plugin's name once.
                           *
                           * The glyph is the **chevron**, not a pencil. This
                           * app already has exactly one mark for "somewhere you
                           * go" — `SettingRow`'s drill-in chevron, one file
                           * over — and configuring a plugin is that: it opens a
                           * second pane with its own fields and its own Apply.
                           * A pencil would be a second vocabulary for the same
                           * act inside the same list, and it would additionally
                           * promise editing *here*, which is not what the
                           * button does. (astryx's own registry has no pencil
                           * either, so drawing one would also have meant a new
                           * glyph in `ui/icon` to say what the existing one
                           * already says.)
                           *
                           * `label` is the whole accessible name now that
                           * nothing is painted — `IconButton` renders it as
                           * `aria-label` — so it keeps the plugin's name in it.
                           * A column of buttons all announced "Configure" is
                           * one a screen reader cannot navigate, which is why
                           * the name was there before the button lost its text
                           * and is why it must not be dropped now.
                           */
                          label={`Configure ${plugin.manifest_name}`}
                          variant="ghost"
                          icon={<Icon name="chevron-right" />}
                          onClick={() => onOpenConfig(plugin.id)}
                        />
                      )}
                      {/* The switch's annotation, set immediately before it —
                          see the header note. `disabled` renders nothing. */}
                      {stateBadge(plugin.state)}
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
