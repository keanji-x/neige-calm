// Settings — workspace preferences. Presentational and props-driven: it never
// calls an API. Loading, saving and error state all arrive as props, and the
// patch it builds leaves through `onSave` (features must not import app).

import { useEffect } from 'react';

import { HTTPS_PROXY_KEY, HTTP_PROXY_KEY, type SettingsPatch } from '../../../../core/domain/settings.ts';
import { Breadcrumb, PageHeader, PageTitle } from '../../ui/page-header/public.tsx';
import { ErrorBox } from '../../ui/error-box/public.tsx';
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

      <div className={styles.form}>
        {/* Section labels take the place of card boxes. Per the boundary
            ladder, a label plus 16px of space separates two groups as well as
            "border + radius + fill + padding" does, using one channel instead
            of four. */}
        <section className={styles.section} aria-labelledby="nc-settings-network">
          <h2 className={styles.sectionLabel} id="nc-settings-network">Network</h2>
          {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
          {!loaded && loadError === null
            ? <p className={styles.hint}>Loading settings…</p>
            : loaded ? (
              <>
                <Field
                  id="nc-settings-http-proxy"
                  label="HTTP proxy"
                  value={draft.http}
                  onChange={(value) => setDraft({ ...draft, http: value })}
                />
                <Field
                  id="nc-settings-https-proxy"
                  label="HTTPS proxy"
                  value={draft.https}
                  onChange={(value) => setDraft({ ...draft, https: value })}
                />
                <div className={styles.actions}>
                  {/*
                    Two kinds of "cannot press", and they must not look alike.
                    A clean form is really `disabled`. Saving is busy: focus is
                    on Save at that moment, and a real `disabled` would throw it
                    away. Reset follows Save, because two buttons in one action
                    row taking two different treatments reads as two different
                    things happening.
                  */}
                  <button
                    type="button"
                    data-nc-action="primary"
                    disabled={!dirty && !saving}
                    aria-busy={saving ? true : undefined}
                    aria-disabled={saving ? true : undefined}
                    data-nc-state={saving ? 'busy' : undefined}
                    onClick={() => { if (saving) return; void onSave(buildPatch(draft, base)); }}
                  >
                    {/* Both labels occupy one grid cell, so the button's width
                        is the wider of the two and never changes. */}
                    <span className="confirm-dialog-label">
                      <span aria-hidden={saving}>Save</span>
                      <span aria-hidden={!saving}>Saving…</span>
                    </span>
                  </button>
                  <button
                    type="button"
                    data-nc-action="secondary"
                    disabled={!dirty && !saving}
                    aria-busy={saving ? true : undefined}
                    aria-disabled={saving ? true : undefined}
                    data-nc-state={saving ? 'busy' : undefined}
                    onClick={() => { if (saving) return; setDraft(base); }}
                  >
                    Reset
                  </button>
                  {/* The only green pixels in the app, and only for four seconds. */}
                  {showSaved && <span className={styles.saved} role="status">Saved.</span>}
                </div>
                {saveError !== null && <p className={styles.error} role="alert">{saveError}</p>}
              </>
            ) : null}
        </section>

        <section className={styles.section} aria-labelledby="nc-settings-appearance">
          <h2 className={styles.sectionLabel} id="nc-settings-appearance">Appearance</h2>
          {/* Deliberately local-only: theme is a device preference, so it never
              goes through onSave. See this module's README.

              `radiogroup`/`radio`, not `tablist`/`tab`: this picks a value, it
              does not switch views, and there is no tabpanel. Calling it a
              tablist would be an accessibility downgrade dressed as a reskin. */}
          <div className={styles.segmented} role="radiogroup" aria-label="Appearance">
            {THEME_MODES.map((mode) => (
              <button
                key={mode}
                type="button"
                role="radio"
                data-nc-role="tab"
                aria-checked={themeMode === mode}
                className={themeMode === mode ? `${styles.segment} ${styles.segmentOn}` : styles.segment}
                onClick={() => onThemeModeChange(mode)}
              >
                {themeLabel(mode)}
              </button>
            ))}
          </div>
          <p className={styles.hint}>Appearance is stored on this device only.</p>
        </section>

        <section className={styles.section} aria-labelledby="nc-settings-about">
          <h2 className={styles.sectionLabel} id="nc-settings-about">About</h2>
          {/* Build-time facts, not API fields (§0.5). `data dir` is kernel-owned
              and has no wire column, so that row is simply absent — no dashed
              box, no "unavailable". */}
          <dl className={styles.about}>
            <dt className={styles.aboutKey}>version</dt>
            <dd className={styles.aboutValue}>{__NC_VERSION__}</dd>
            <dt className={styles.aboutKey}>build</dt>
            <dd className={styles.aboutValue}>{__NC_BUILD__}</dd>
          </dl>
        </section>
      </div>
    </div>
  );
}

function Field({ id, label, value, onChange }: {
  id: string;
  label: string;
  value: string;
  onChange: (value: string) => void;
}) {
  return (
    <div className={styles.field}>
      <label className={styles.label} htmlFor={id}>{label}</label>
      <input
        id={id}
        className={styles.input}
        type="text"
        value={value}
        spellCheck={false}
        autoComplete="off"
        onChange={(event) => onChange(event.target.value)}
      />
    </div>
  );
}
