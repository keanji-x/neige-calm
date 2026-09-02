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
// owns the dialog, exactly as it already owns the New wave dialog and for the
// same reason — two surfaces, one dialog, one set of strings.
//
// The routes stay real routes (`/settings`, `/settings/templates`,
// `/settings/templates/$templateId`, `/settings/plugins`) and their components
// render nothing. The URL is the *state* — which section is open, which
// template is being edited, what a deep link means, what Back does — and this
// file is the *view* of that state. Splitting them that way is what lets Back
// leave the template editor instead of leaving Settings while the panel around
// it never blinks.

import { useQuery } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { PluginsPane } from '../../features/settings/plugins.tsx';
import {
  AboutPane, AppearancePane, NetworkPane, SettingsSurface,
  type SettingsSection, type ThemeMode as SettingsThemeMode,
} from '../../features/settings/public.tsx';
import { TemplateEditorPage, TemplateListPage } from '../../features/settings/templates.tsx';
import { Dialog } from '../../ui/dialog/public.tsx';
import { useState } from '../../ui/state/public.ts';
import {
  pluginsQueryOptions, settingsQueryOptions, usePluginMutations, useSettingsMutation,
  useWaveTemplateMutation, useWaveTemplates,
} from '../providers/queries.ts';
import { routeParamFromPath, useCurrentPath, useGo, type NavTarget } from '../router/navigation.ts';
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
  if (path === '/settings/templates' || path.startsWith('/settings/templates/')) return 'templates';
  return null;
}

/** The route each nav entry navigates to. `network` is the bare `/settings`. */
function targetForSection(section: SettingsSection): NavTarget {
  switch (section) {
    case 'network': return { name: 'settings' };
    case 'appearance': return { name: 'settings-appearance' };
    case 'templates': return { name: 'settings-templates' };
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
          path={path}
          transport={transport}
          unauthorized={unauthorized}
        />
      </SettingsSurface>
    </Dialog>
  );
}

/** One switch, so a new section cannot forget to be rendered. */
function SectionPane({ section, path, transport, unauthorized }: SettingsOverlayProps & {
  section: SettingsSection;
  path: string;
}) {
  switch (section) {
    case 'appearance': return <AppearancePaneHost />;
    case 'templates': return <TemplatesPaneHost path={path} transport={transport} unauthorized={unauthorized} />;
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
 */
function PluginsPaneHost({ transport, unauthorized }: SettingsOverlayProps) {
  const plugins = useQuery(pluginsQueryOptions(transport, unauthorized));
  const mutations = usePluginMutations(transport, unauthorized);
  return (
    <PluginsPane
      plugins={plugins.data}
      loadError={plugins.error instanceof Error ? plugins.error.message : null}
      onRetryLoad={() => { void plugins.refetch(); }}
      pendingIds={mutations.pendingIds}
      errors={mutations.errors}
      onSetEnabled={mutations.setEnabled}
    />
  );
}

/**
 * Settings › Templates (#1230) — the list, or one template's editor.
 *
 * Both read the **template list** — the same read the New wave picker uses.
 * There is no per-template endpoint: the list already carries `id` / `title` /
 * `tasks[{key, goal}]`, and a second read would be a second authority for the
 * same facts plus an N+1 whose failure modes have to be reasoned about apart
 * from the list's.
 */
function TemplatesPaneHost({ path, transport, unauthorized }: SettingsOverlayProps & { path: string }) {
  const go = useGo();
  const templates = useWaveTemplates(transport, unauthorized);
  const templateId = routeParamFromPath(path, '/settings/templates/');
  if (templateId === undefined) {
    return (
      <TemplateListPage
        templates={templates.loaded ? templates.templates : undefined}
        loadError={templates.error}
        onRetryLoad={templates.refetch}
        onEdit={(id) => go({ name: 'settings-template', templateId: id })}
      />
    );
  }
  /*
   * Keyed on the id: `saving` / `saveError` / `savedAt` are per-template facts,
   * and this host is reused across ids. Without the key, one template's
   * "Saved." and error banner render over the next template's editor, and an
   * in-flight save suppresses the re-seed so template A's rows show under
   * template B's title.
   */
  return (
    <TemplateEditorSave
      key={templateId}
      templateId={templateId}
      transport={transport}
      unauthorized={unauthorized}
    />
  );
}

function TemplateEditorSave({ templateId, transport, unauthorized }: SettingsOverlayProps & {
  templateId: string;
}) {
  const go = useGo();
  const templates = useWaveTemplates(transport, unauthorized);
  const saveTemplate = useWaveTemplateMutation(transport, unauthorized);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [savedAt, setSavedAt] = useState<number | null>(null);
  const template = templates.loaded
    ? templates.templates.find((entry) => entry.id === templateId)
    : undefined;
  return (
    <TemplateEditorPage
      template={template}
      /* An id that names no template is a load failure for this pane, not an
         empty editor: blank fields for a template that does not exist would
         invite a save against nothing. */
      loadError={templates.loaded && template === undefined
        ? `No template named ${templateId}.`
        : templates.error}
      onRetryLoad={templates.refetch}
      saving={saving}
      saveError={saveError}
      savedAt={savedAt}
      onOpenTemplates={() => go({ name: 'settings-templates' })}
      onSave={(save) => {
        setSaving(true);
        setSaveError(null);
        return saveTemplate(save)
          .then(() => { setSavedAt(Date.now()); })
          .catch((error: unknown) => {
            setSaveError(error instanceof Error ? error.message : 'Save failed.');
          })
          .finally(() => { setSaving(false); });
      }}
    />
  );
}
