// One Settings visit: a desktop dialog or a mobile route page, mounted above the
// shell's keyed route stage so section routes keep one surface. The URL selects the section.

import { useCallback, useEffect, useRef } from 'react';
import { createPortal } from 'react-dom';
import { useRouter, useRouterState, type RouterHistory } from '@tanstack/react-router';
import { useQuery } from '@tanstack/react-query';

import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { PluginConfigPane } from '../../features/settings/plugin-config.tsx';
import { PluginAddPane } from '../../features/settings/plugin-add.tsx';
import { PlannersPane } from '../../features/settings/planners.tsx';
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
import { agentProvidersQueryOptions, useAgentProvidersRecheck } from '../providers/agent-providers.ts';
import { useCurrentPath, useGo, type NavTarget } from '../router/navigation.ts';
import { useTheme } from '../theme/public.tsx';
import { MobileAccessHost } from './mobile-access-host.tsx';

/** Which pane the path asks for, or `null` when the reader is not in Settings. */
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
 * The pane tree is one portal into a stable container; only the container's DOM
 * parent changes between the mobile page and the desktop dialog slot, so resizing
 * cannot discard plugin drafts. DOM ancestry is not React context here.
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
    case 'planners': return <PlannersPaneHost transport={transport} unauthorized={unauthorized} />;
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

/** Settings › Planners: the shared availability answer the new-track picker also reads, and its Recheck. */
function PlannersPaneHost({ transport, unauthorized }: SettingsOverlayProps) {
  const providers = useQuery(agentProvidersQueryOptions(transport, unauthorized));
  const recheck = useAgentProvidersRecheck(transport, unauthorized);
  return (
    <PlannersPane
      providers={providers.data}
      loadError={providers.error instanceof Error ? providers.error.message : null}
      onRetryLoad={() => { void providers.refetch(); }}
      onRecheck={recheck.recheck}
      rechecking={recheck.rechecking}
      recheckError={recheck.error}
    />
  );
}

function AppearancePaneHost() {
  const theme = useTheme();
  return (
    <AppearancePane
      // `app/theme` and `features/settings` each own their copy of the mode union
      // (features may not import app); the two are only kept in step by this line.
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
      /* The pane follows each commit's own promise, so a failure lands on the row
               that failed; one shared triple for two rows put HTTP's failure on the HTTPS row. */
      onSave={(patch) => save(patch).then(() => undefined)}
    />
  );
}

/**
 * Settings › Plugins. Which row's configuration is open is held here, not in the
 * URL: the second level holds an operator's unsaved edits, so a shareable link
 * could only arrive with someone else's draft or empty.
 */
function PluginsPaneHost({ transport, unauthorized }: SettingsOverlayProps) {
  const plugins = useQuery(pluginsQueryOptions(transport, unauthorized));
  const mutations = usePluginMutations(transport, unauthorized);
  const config = usePluginConfigMutations(transport, unauthorized);
  const install = usePluginInstall(transport, unauthorized);
  const [openId, setOpenId] = useState<string | null>(null);
  /* The install form is a second level for the same reason: it holds what the
       operator is typing, including a credential. */
  const [adding, setAdding] = useState(false);
  /* The row is re-derived from the list on every render, never copied into state:
   * the list refetches, and a plugin can be uninstalled from elsewhere. */
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
        /* The list is what says the install worked; the new row is already on the
                   screen behind the form, switched off. */
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
