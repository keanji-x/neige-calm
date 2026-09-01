// Settings — workspace preferences. Presentational and props-driven: it never
// calls an API. Loading, saving and error state all arrive as props, and the
// patch it builds leaves through `onSave` (features must not import app).

import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { Card as AstryxCard } from '@astryxdesign/core/Card';
import {
  MetadataList as AstryxMetadataList,
  MetadataListItem as AstryxMetadataListItem,
} from '@astryxdesign/core/MetadataList';
import {
  SegmentedControl as AstryxSegmentedControl,
  SegmentedControlItem as AstryxSegmentedControlItem,
} from '@astryxdesign/core/SegmentedControl';
import { TextInput as AstryxTextInput } from '@astryxdesign/core/TextInput';
import { useEffect } from 'react';

import { HTTPS_PROXY_KEY, HTTP_PROXY_KEY, type SettingsPatch } from '../../../../core/domain/settings.ts';
import { Breadcrumb, PageHeader, PageTitle } from '../../ui/page-header/public.tsx';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { MobileHeader } from '../../ui/mobile-header/public.tsx';
import { useState } from '../../ui/state/public.ts';
import styles from './settings.module.css';

/**
 * Mirrors `app/theme`'s mode union by value. `features/**` must not import
 * `app/**`, so the union is declared here and the app layer adapts to it; the
 * two are kept in step by the router wiring, not by a type import.
 */
export type ThemeMode = 'light' | 'dark' | 'system';

export type SettingsPageProps = Readonly<{
  /** `undefined` means "still loading" — never render an empty form for it. */
  settings: Readonly<Record<string, string>> | undefined;
  loadError: string | null;
  saving: boolean;
  saveError: string | null;
  /** Timestamp of the last successful save; drives the transient confirmation. */
  savedAt: number | null;
  onSave: (patch: SettingsPatch) => void | Promise<void>;
  onRetryLoad: () => void;
  onOpenToday: () => void;
  themeMode: ThemeMode;
  onThemeModeChange: (mode: ThemeMode) => void;
  /** Tests shorten the confirmation window; production uses the default. */
  savedNoticeMs?: number;
}>;

const THEME_MODES = Object.freeze(['light', 'dark', 'system'] as const);
const SAVED_NOTICE_MS = 4000;

type Draft = { http: string; https: string };

function themeLabel(mode: ThemeMode): string {
  return mode === 'light' ? 'Light' : mode === 'dark' ? 'Dark' : 'System';
}

/**
 * INV-SETTINGS-001 — a field the user cleared is sent as `null`, never `''`.
 * The kernel deletes a key for either value, so the two converge; sending
 * `null` states the intent instead of leaning on that equivalence. Unchanged
 * keys are absent entirely, so a save never rewrites a value nobody touched.
 */
function buildPatch(draft: Draft, seed: Draft): SettingsPatch {
  const patch: Record<string, string | null> = {};
  if (draft.http !== seed.http) patch[HTTP_PROXY_KEY] = draft.http === '' ? null : draft.http;
  if (draft.https !== seed.https) patch[HTTPS_PROXY_KEY] = draft.https === '' ? null : draft.https;
  return patch;
}

export function SettingsPage({
  settings, loadError, saving, saveError, savedAt, onSave, onRetryLoad, onOpenToday,
  themeMode, onThemeModeChange, savedNoticeMs = SAVED_NOTICE_MS,
}: SettingsPageProps) {
  const loaded = settings !== undefined;
  const incoming: Draft = {
    http: settings?.[HTTP_PROXY_KEY] ?? '',
    https: settings?.[HTTPS_PROXY_KEY] ?? '',
  };

  // Seeding compares by *value*, not by object identity: a parent that hands
  // back a fresh object on every render (a query cache does) must not wipe out
  // what the user is typing. A genuine server change does re-seed.
  const [seed, setSeed] = useState<Draft | null>(null);
  const [draft, setDraft] = useState<Draft>({ http: '', https: '' });
  if (loaded && (seed === null || seed.http !== incoming.http || seed.https !== incoming.https)) {
    const previous = seed;
    setSeed(incoming);
    setDraft((current) => ({
      http: previous === null || current.http === previous.http ? incoming.http : current.http,
      https: previous === null || current.https === previous.https ? incoming.https : current.https,
    }));
  }

  const [acknowledged, setAcknowledged] = useState<number | null>(null);
  useEffect(() => {
    if (savedAt === null) return;
    const id = setTimeout(() => setAcknowledged(savedAt), savedNoticeMs);
    return () => clearTimeout(id);
  }, [savedAt, savedNoticeMs]);

  const base = seed ?? { http: '', https: '' };
  const dirty = draft.http !== base.http || draft.https !== base.https;
  const showSaved = savedAt !== null && acknowledged !== savedAt;

  return (
    <div className={styles.page}>
      {/* Two rows: breadcrumb + title, no machine identity. --header-h is 62. */}
      <PageHeader
        breadcrumb={<Breadcrumb ancestor="Today" onNavigate={onOpenToday} />}
        title={<PageTitle>Settings</PageTitle>}
      />
      <div className={styles.mobileHeader}><MobileHeader title="Settings" level={1} /></div>

      <div className={styles.form}>
        <AstryxCard className={styles.sectionCard} padding={4} data-nc-settings-card="">
          <section className={styles.section} aria-labelledby="nc-settings-network">
            <h2 className={styles.sectionLabel} id="nc-settings-network">Network</h2>
            {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
            {!loaded && loadError === null
              ? <p className={styles.hint}>Loading settings…</p>
              : loaded ? (
                <>
                  <AstryxTextInput
                    label="HTTP proxy"
                    value={draft.http}
                    onChange={(value) => setDraft({ ...draft, http: value })}
                    size="lg"
                    width="100%"
                  />
                  <AstryxTextInput
                    label="HTTPS proxy"
                    value={draft.https}
                    onChange={(value) => setDraft({ ...draft, https: value })}
                    size="lg"
                    width="100%"
                  />
                  <div className={styles.actions}>
                    <AstryxButton
                      label={saving ? 'Saving…' : 'Save'}
                      variant="primary"
                      size="lg"
                      isDisabled={!dirty && !saving}
                      isLoading={saving}
                      isInterruptible
                      data-nc-state={saving ? 'busy' : undefined}
                      onClick={() => { if (saving) return; void onSave(buildPatch(draft, base)); }}
                    />
                    <AstryxButton
                      label="Reset"
                      variant="secondary"
                      size="lg"
                      isDisabled={!dirty || saving}
                      data-nc-state={saving ? 'busy' : undefined}
                      onClick={() => { if (saving) return; setDraft(base); }}
                    />
                    {/*
                      * `data-nc-settings-saved` is the e2e seam, not decoration.
                      * `role="status"` cannot locate this span: every Astryx
                      * `Button` renders its own unconditional, empty-text
                      * `role="status"` live region for loading announcements
                      * (`@astryxdesign/core` `Button.tsx`), so the two buttons
                      * beside this one make `getByRole('status')` resolve to
                      * three elements. Filtering those by the text we are about
                      * to assert would be circular — it could only prove "some
                      * status says Saved.", never "the save succeeded". The
                      * anchor keeps locating independent of asserting.
                      */}
                    {showSaved && (
                      <span className={styles.saved} role="status" data-nc-settings-saved>Saved.</span>
                    )}
                  </div>
                  {saveError !== null && <p className={styles.error} role="alert">{saveError}</p>}
                </>
              ) : null}
          </section>
        </AstryxCard>

        <AstryxCard className={styles.sectionCard} padding={4} data-nc-settings-card="">
          <section className={styles.section} aria-labelledby="nc-settings-appearance">
            <h2 className={styles.sectionLabel} id="nc-settings-appearance">Appearance</h2>
            <AstryxSegmentedControl
              value={themeMode}
              onChange={(value) => onThemeModeChange(value === 'light' || value === 'dark' ? value : 'system')}
              label="Appearance"
              size="lg"
              layout="fill"
            >
              {THEME_MODES.map((mode) => (
                <AstryxSegmentedControlItem key={mode} value={mode} label={themeLabel(mode)} />
              ))}
            </AstryxSegmentedControl>
            <p className={styles.hint}>Stored on this device.</p>
          </section>
        </AstryxCard>

        <AstryxCard className={styles.sectionCard} padding={4} data-nc-settings-card="">
          <section className={styles.section} aria-labelledby="nc-settings-about">
            <h2 className={styles.sectionLabel} id="nc-settings-about">About</h2>
            <AstryxMetadataList columns="single" label={{ position: 'start', width: '6rem' }}>
              <AstryxMetadataListItem label="Version">
                <span className={styles.aboutValue}>{__NC_VERSION__}</span>
              </AstryxMetadataListItem>
              <AstryxMetadataListItem label="Build">
                <span className={styles.aboutValue}>{__NC_BUILD__}</span>
              </AstryxMetadataListItem>
            </AstryxMetadataList>
          </section>
        </AstryxCard>
      </div>
    </div>
  );
}
