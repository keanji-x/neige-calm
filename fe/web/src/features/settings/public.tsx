// Settings — workspace preferences. Presentational and props-driven: it never
// calls an API. Loading, saving and error state all arrive as props, and the
// patch it builds leaves through `onSave` (features must not import app).

import { useEffect } from 'react';

import { HTTPS_PROXY_KEY, HTTP_PROXY_KEY, type SettingsPatch } from '../../../../core/domain/settings.ts';
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
  settings, loadError, saving, saveError, savedAt, onSave, onOpenToday,
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
    setSeed(incoming);
    setDraft(incoming);
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
      <nav className={styles.crumbs} aria-label="Breadcrumb">
        {/* INV-A11Y-061 — in-app navigation is a button + callback, never <a href>. */}
        <button type="button" className={styles.crumbLink} onClick={onOpenToday}>Today</button>
        <span className={styles.crumbSep} aria-hidden="true">/</span>
        <span className={styles.crumb} aria-current="page">Settings</span>
      </nav>

      <h1 className={styles.title}>Settings</h1>

      <section className={styles.card} aria-labelledby="nc-settings-network">
        <h2 className={styles.cardTitle} id="nc-settings-network">Network</h2>
        {loadError !== null && <p className={styles.error} role="alert">{loadError}</p>}
        {!loaded
          ? <p className={styles.loading}>Loading settings…</p>
          : (
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
                <button
                  type="button"
                  className={styles.primary}
                  disabled={!dirty || saving}
                  onClick={() => void onSave(buildPatch(draft, base))}
                >
                  {saving ? 'Saving…' : 'Save'}
                </button>
                <button
                  type="button"
                  className={styles.secondary}
                  disabled={!dirty || saving}
                  onClick={() => setDraft(base)}
                >
                  Reset
                </button>
                {showSaved && <span className={styles.saved} role="status">Saved.</span>}
              </div>
              {saveError !== null && <p className={styles.error} role="alert">{saveError}</p>}
            </>
          )}
      </section>

      <section className={styles.card} aria-labelledby="nc-settings-appearance">
        <h2 className={styles.cardTitle} id="nc-settings-appearance">Appearance</h2>
        {/* Deliberately local-only: theme is a device preference, so it never
            goes through onSave. See this module's README. */}
        <div className={styles.radios} role="radiogroup" aria-label="Appearance">
          {THEME_MODES.map((mode) => (
            <button
              key={mode}
              type="button"
              role="radio"
              aria-checked={themeMode === mode}
              className={themeMode === mode ? `${styles.radio} ${styles.radioOn}` : styles.radio}
              onClick={() => onThemeModeChange(mode)}
            >
              {themeLabel(mode)}
            </button>
          ))}
        </div>
        <p className={styles.hint}>Appearance is stored on this device only.</p>
      </section>
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
