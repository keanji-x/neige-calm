// The Settings overlay — one dialog, four routes, and the reads each pane needs.
//
// ## Why this lives in the shell and not in the route components
//
// It used to be a `<Dialog>` returned by each settings route. That renders a
// *new* dialog per route: clicking General → Plugins unmounted one panel and
// mounted another, so the entrance animation replayed on every click and the
// panel flashed. Measured, not guessed — after a section click the panel was a
// different DOM node with `animation-name: dialog-enter` running again.
//
// A dialog that stays open while the reader moves *inside* it has to outlive
// those navigations, and the nearest thing that does is the shell: it is above
// `<Outlet />` and never unmounts across a settings navigation. So the shell
// owns the dialog, exactly as it already owns the New track dialog and for the
// same reason — two surfaces, one dialog, one set of strings.
//
// The routes stay real routes (`/settings`, `/settings/appearance`,
// `/settings/plugins`, `/settings/about`) and their components render nothing.
// The URL is the *state* — which section is open, what a deep link means, what
// Back does — and this file is the *view* of that state. Splitting them that
// way is what keeps the panel from blinking as the reader moves between
// sections.
//
// #1300 S1 removed `/settings/templates` and `/settings/templates/$templateId`
// along with the template editor they existed for; templates are a read-only
// recipe again, and `GET /api/track-templates` is read only by the New track
// picker.

import { useQuery } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { PluginConfigPane } from '../../features/settings/plugin-config.tsx';
import { PluginsPane } from '../../features/settings/plugins.tsx';
import {
  AboutPane, AppearancePane, NetworkPane, SettingsSurface,
  type SettingsSection, type ThemeMode as SettingsThemeMode,
} from '../../features/settings/public.tsx';
import { Dialog } from '../../ui/dialog/public.tsx';
import { useState } from '../../ui/state/public.ts';
import {
  pluginDetailQueryOptions, pluginsQueryOptions, settingsQueryOptions, usePluginConfigMutations,
  usePluginMutations, useSettingsMutation,
} from '../providers/queries.ts';
import { useCurrentPath, useGo, type NavTarget } from '../router/navigation.ts';
import { useTheme } from '../theme/public.tsx';

/**
 * Which pane the path asks for, or `null` when the reader is not in Settings.
 *
 * Exported and pure so the mapping is directly assertable: every settings path
 * has to reach a pane, and no other path may open the dialog.
 */
export function settingsSectionForPath(path: string): SettingsSection | null {
  if (path === '/settings') return 'network';
  if (path === '/settings/appearance') return 'appearance';
  if (path === '/settings/plugins') return 'plugins';
  if (path === '/settings/about') return 'about';
  return null;
}

/** The route each nav entry navigates to. `network` is the bare `/settings`. */
function targetForSection(section: SettingsSection): NavTarget {
  switch (section) {
    case 'network': return { name: 'settings' };
    case 'appearance': return { name: 'settings-appearance' };
    case 'plugins': return { name: 'settings-plugins' };
    case 'about': return { name: 'settings-about' };
  }
}

export type SettingsOverlayProps = Readonly<{
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
}>;

export function SettingsOverlay({ transport, unauthorized }: SettingsOverlayProps) {
  const go = useGo();
  const path = useCurrentPath();
  const section = settingsSectionForPath(path);
  return (
    <Dialog
      open={section !== null}
      // Today, not `history.back()`: a cold-start deep link to
      // `/settings/plugins` has nothing to go back to, and walking out of the
      // application is not "close this dialog".
      onClose={() => go({ name: 'today' })}
      title="Settings"
      wide
    >
      <SettingsSurface
        section={section ?? 'network'}
        onSelectSection={(next) => go(targetForSection(next))}
      >
        <SectionPane
          section={section ?? 'network'}
          transport={transport}
          unauthorized={unauthorized}
        />
      </SettingsSurface>
    </Dialog>
  );
}

/** One switch, so a new section cannot forget to be rendered. */
function SectionPane({ section, transport, unauthorized }: SettingsOverlayProps & {
  section: SettingsSection;
}) {
  switch (section) {
    case 'appearance': return <AppearancePaneHost />;
    case 'plugins': return <PluginsPaneHost transport={transport} unauthorized={unauthorized} />;
    case 'about': return <AboutPane />;
    case 'network': return <NetworkPaneHost transport={transport} unauthorized={unauthorized} />;
  }
}

function AppearancePaneHost() {
  const theme = useTheme();
  return (
    <AppearancePane
      // `app/theme` and `features/settings` each own their copy of the mode
      // union — features may not import app. The adaptation is here, and the
      // two unions are only kept in step by this line.
      themeMode={theme.mode satisfies SettingsThemeMode}
      onThemeModeChange={(mode) => theme.setMode(mode)}
    />
  );
}

function NetworkPaneHost({ transport, unauthorized }: SettingsOverlayProps) {
  const save = useSettingsMutation(transport, unauthorized);
  const settings = useQuery(settingsQueryOptions(transport, unauthorized));
  return (
    <NetworkPane
      settings={settings.data?.settings}
      loadError={settings.error instanceof Error ? settings.error.message : null}
      onRetryLoad={() => { void settings.refetch(); }}
      /* The promise is the whole contract: the pane follows each commit's own
         request, so a failure lands on the row that failed. This host holds no
         `saving` / `saveError` / `savedAt` of its own — one triple for two rows
         is what put HTTP's failure on the HTTPS row. */
      onSave={(patch) => save(patch).then(() => undefined)}
    />
  );
}

/**
 * Settings › Plugins — the installed list, read here and rendered there.
 *
 * The list is not primed by a route loader: it is one screen's read, it fails
 * loudly on its own (`retry: false`), and a loader would make opening any other
 * settings pane wait on it.
 *
 * ## The configuration pane is a second level, not a second route
 *
 * #1284 S4 adds a drill-in: a row whose plugin declares a `config_schema` walks
 * into that plugin's configuration. Which row is open is held here rather than
 * in the URL, and that is the one place this file departs from "the URL is the
 * state".
 *
 * The reason is what the second level *is*. Every other settings level is a
 * screen you can be sent to; this one holds an operator's unsaved edits to a
 * document, keyed to a schema version the kernel may have replaced since. A
 * shareable link to it would either arrive with someone else's draft or arrive
 * empty on a plugin that no longer declares the field the link was made for. So
 * it lives for as long as the visit does, and `/settings/plugins` stays the
 * address of the list.
 */
function PluginsPaneHost({ transport, unauthorized }: SettingsOverlayProps) {
  const plugins = useQuery(pluginsQueryOptions(transport, unauthorized));
  const mutations = usePluginMutations(transport, unauthorized);
  const config = usePluginConfigMutations(transport, unauthorized);
  const [openId, setOpenId] = useState<string | null>(null);
  /*
   * The row is what says the pane may be open at all, and it is re-derived from
   * the list on every render rather than copied into state when it was clicked.
   * The list refetches — an enable/disable invalidates it, and a plugin can be
   * uninstalled from elsewhere — so a pane that trusted a captured row would
   * keep offering a configuration screen for a plugin that is no longer there,
   * and `has_config` is the kernel's answer to "is there anything to configure",
   * not a fact about the moment the button was pressed.
   */
  const open = openId === null
    ? undefined
    : plugins.data?.find((plugin) => plugin.id === openId && plugin.has_config);
  const detail = useQuery({
    ...pluginDetailQueryOptions(transport, openId ?? '', unauthorized),
    enabled: open !== undefined,
  });

  if (openId !== null && open !== undefined) {
    return (
      <PluginConfigPane
        pluginId={open.id}
        pluginName={open.manifest_name}
        enabled={open.enabled}
        detail={detail.data}
        loadError={detail.error instanceof Error ? detail.error.message : null}
        onRetryLoad={() => { void detail.refetch(); }}
        onBack={() => setOpenId(null)}
        onSave={(patch, options) => config.save(open.id, patch, options)}
        onApplyRestart={(patch, options) => config.applyRestart(open.id, patch, options)}
      />
    );
  }

  return (
    <PluginsPane
      plugins={plugins.data}
      loadError={plugins.error instanceof Error ? plugins.error.message : null}
      onRetryLoad={() => { void plugins.refetch(); }}
      pendingIds={mutations.pendingIds}
      errors={mutations.errors}
      onSetEnabled={mutations.setEnabled}
      onOpenConfig={setOpenId}
    />
  );
}

