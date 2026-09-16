// One Settings visit: a desktop dialog or a mobile route page, selected by the
// shared compact breakpoint. The shell owns this above its keyed route stage,
// so section routes keep one surface mounted. The URL selects the section;
// pane hosts remain the sole owners of API reads and unsaved plugin forms.

import { useCallback, useEffect, useRef } from 'react';
import { createPortal } from 'react-dom';
import { useRouter, useRouterState, type RouterHistory } from '@tanstack/react-router';
import { useQuery } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { PluginConfigPane } from '../../features/settings/plugin-config.tsx';
import { PluginAddPane } from '../../features/settings/plugin-add.tsx';
import { PluginsPane } from '../../features/settings/plugins.tsx';
import {
  AboutPane, AppearancePane, GeneralPane, NetworkPane, SettingsSurface,
  type ThemeMode as SettingsThemeMode,
} from '../../features/settings/public.tsx';
import { SETTINGS_SECTIONS, settingsSectionLabel, type SettingsSection } from '../../features/settings/navigation.tsx';
import { Dialog } from '../../ui/dialog/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { useCompactViewport } from '../../ui/viewport/public.ts';
import { MobileHeader } from '../../ui/mobile-header/public.tsx';
import styles from './settings-page.module.css';
import {
  pluginDetailQueryOptions, pluginsQueryOptions, settingsQueryOptions, usePluginConfigMutations,
  usePluginInstall, usePluginMutations, useSettingsMutation,
} from '../providers/queries.ts';
import { useCurrentPath, useGo, type NavTarget } from '../router/navigation.ts';
import { useTheme } from '../theme/public.tsx';
import { MobileAccessHost } from './mobile-access-host.tsx';

/**
 * Which pane the path asks for, or `null` when the reader is not in Settings.
 *
 * Exported and pure so the mapping is directly assertable: every settings path
 * has to reach a pane, and no other path may open the dialog.
 */
export function settingsSectionForPath(path: string): SettingsSection | null {
  if (path === '/settings') return 'general';
  return SETTINGS_SECTIONS.find((entry) => path === `/settings/${entry.id}`)?.id ?? null;
}

/** Desktop General keeps the historical root; every mobile category has a URL. */
function targetForSection(section: SettingsSection, compact: boolean): NavTarget {
  return { name: section === 'general' && !compact ? 'settings' : `settings-${section}` };
}

export type SettingsOverlayProps = Readonly<{
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
}>;

/**
 * The pane tree is one portal into a stable container. Only the container's DOM
 * parent changes between the inline mobile page and the desktop dialog slot.
 * React ownership remains here, so resizing cannot discard plugin drafts.
 * Current Settings panes use no Dialog child-view context; any future such
 * consumer needs an explicit bridge rather than assuming DOM ancestry is context.
 */
export function SettingsOverlay({ transport, unauthorized }: SettingsOverlayProps) {
  const go = useGo();
  const router = useRouter();
  const path = useCurrentPath();
  const href = useRouterState({ select: (state) => state.location.href });
  const section = settingsSectionForPath(path);
  const compact = useCompactViewport();
  const mobileIndex = compact && path === '/settings';
  const [contentHost] = useState(() => document.createElement('div'));
  const focusedContentRef = useRef<HTMLElement | null>(null);
  const relocatingContentRef = useRef(false);
  const attachContent = useCallback((slot: HTMLDivElement | null) => {
    if (slot === null || contentHost.parentNode === slot) return;
    const focused = focusedContentRef.current;
    const restore = focused !== null && contentHost.contains(focused)
      && (document.activeElement === focused || document.activeElement === document.body);
    // Moving the same editor between responsive hosts is not leaving its field.
    relocatingContentRef.current = true;
    slot.appendChild(contentHost);
    if (restore) focused.focus({ preventScroll: true });
    relocatingContentRef.current = false;
  }, [contentHost]);
  useEffect(() => {
    // On desktop → phone, Dialog removes background inertness in its cleanup.
    // A focus attempt while moving the field can therefore have been ignored.
    const focused = focusedContentRef.current;
    if (compact && focused !== null && contentHost.contains(focused)
      && document.activeElement === document.body) focused.focus({ preventScroll: true });
  }, [compact, contentHost]);
  const history = router.history as RouterHistory;
  const historyIndex = history.location.state.__TSR_index;
  const returnLocation = useRef<Readonly<{ href: string; index: number }> | null>(null);
  const indexLocation = useRef<number | null>(null);
  useEffect(() => {
    if (section === null) {
      returnLocation.current = { href, index: historyIndex };
      indexLocation.current = null;
    } else if (path === '/settings') indexLocation.current = historyIndex;
  }, [href, historyIndex, path, section]);
  const leaveMobilePage = () => {
    const previous = returnLocation.current;
    const distance = previous === null ? 0 : history.location.state.__TSR_index - previous.index;
    // Only pop entries after a workspace entry this mounted shell observed.
    // Section pushes made on desktop belong to the same visit if resized.
    if (distance > 0 && history.canGoBack()) {
      history.go(-distance);
      return;
    }
    // A cold settings link, or a replaced workspace entry, has no owned
    // entry to pop. Keep that exit inside the app without guessing at Back.
    void router.navigate({ to: previous?.href ?? '/', replace: true });
  };
  const backToIndex = () => {
    const previous = indexLocation.current;
    const distance = previous === null ? 0 : history.location.state.__TSR_index - previous;
    if (distance > 0 && history.canGoBack()) {
      history.go(-distance);
      return;
    }
    // Cold category links and desktop shortcuts have no observed index entry.
    go({ name: 'settings' }, { replace: true });
  };
  if (section === null) return null;
  return <>
    <section className={styles.page} hidden={!compact} data-nc-settings-page="" aria-label="Settings">
      <MobileHeader title={mobileIndex ? 'Settings' : settingsSectionLabel(section)} level={1}
        backLabel={mobileIndex ? 'workspace' : 'Settings'} onBack={mobileIndex ? leaveMobilePage : backToIndex} />
      <div className={styles.content} ref={compact ? attachContent : undefined} />
    </section>
    <Dialog open={!compact} onClose={() => go({ name: 'today' })} title="Settings" wide>
      <div ref={!compact ? attachContent : undefined} />
    </Dialog>
    {createPortal(<div
      onFocusCapture={(event) => { focusedContentRef.current = event.target; }}
      onBlurCapture={(event) => {
        if (relocatingContentRef.current) event.stopPropagation();
        else if (!contentHost.contains(event.relatedTarget)) focusedContentRef.current = null;
      }}>
    <SettingsSurface section={section}
      presentation={!compact ? 'desktop' : mobileIndex ? 'mobile-index' : 'mobile-detail'}
      onSelectSection={(next) => go(targetForSection(next, compact))}>
      <SectionPane section={section} transport={transport} unauthorized={unauthorized} />
    </SettingsSurface></div>, contentHost)}
  </>;
}

/** One switch, so a new section cannot forget to be rendered. */
function SectionPane({ section, transport, unauthorized }: SettingsOverlayProps & {
  section: SettingsSection;
}) {
  switch (section) {
    case 'general': return <GeneralPaneHost transport={transport} unauthorized={unauthorized} />;
    case 'appearance': return <AppearancePaneHost />;
    case 'plugins': return <PluginsPaneHost transport={transport} unauthorized={unauthorized} />;
    case 'about': return <AboutPane />;
    case 'network': return <NetworkPaneHost transport={transport} unauthorized={unauthorized} />;
  }
}

function GeneralPaneHost({ transport, unauthorized }: SettingsOverlayProps) {
  const save = useSettingsMutation(transport, unauthorized);
  const settings = useQuery(settingsQueryOptions(transport, unauthorized));
  return (
    <GeneralPane
      settings={settings.data?.settings}
      loadError={settings.error instanceof Error ? settings.error.message : null}
      onRetryLoad={() => { void settings.refetch(); }}
      onSave={(patch) => save(patch).then(() => undefined)}
    />
  );
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
  const [mobileOpen, setMobileOpen] = useState(false);
  const save = useSettingsMutation(transport, unauthorized);
  const settings = useQuery(settingsQueryOptions(transport, unauthorized));
  if (mobileOpen) return <MobileAccessHost transport={transport} unauthorized={unauthorized} onBack={() => setMobileOpen(false)} />;
  return (
    <NetworkPane
      onOpenMobile={() => setMobileOpen(true)}
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
  const install = usePluginInstall(transport, unauthorized);
  const [openId, setOpenId] = useState<string | null>(null);
  /* #1480 — the install form is a second level for the same reason the
     configuration pane is one: it holds what the operator is typing, including
     a credential, and a shareable link to it could only arrive empty or with
     somebody else's draft in it. */
  const [adding, setAdding] = useState(false);
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

  if (adding) {
    return (
      <PluginAddPane
        pending={install.pending}
        onBack={() => setAdding(false)}
        onCheckConnector={install.checkConnector}
        onInstallConnector={install.installConnector}
        onInstallLocalPath={install.installLocalPath}
        /* The list is what says the install worked, so the form leaves as soon
           as the kernel accepts one — the new row is already on the screen
           behind it, switched off, waiting to be enabled. */
        onInstalled={() => setAdding(false)}
      />
    );
  }

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
      effectBoundaryIds={mutations.effectBoundaryIds}
      onSetEnabled={mutations.setEnabled}
      onOpenConfig={setOpenId}
      onAdd={() => setAdding(true)}
      onUninstall={mutations.uninstall}
    />
  );
}
