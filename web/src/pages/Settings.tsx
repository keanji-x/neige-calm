// SettingsPage — app-global Settings view, reachable from the TitleBar
// gear button (`/settings`).
//
// First config block is HTTP/HTTPS proxy. The codex spawn path reads
// these at process-spawn time and overrides whatever proxy the docker
// container exports by default (see `routes::codex_cards::create_codex_card`).
//
// The page sources `useSettingsQuery()` for the snapshot and reflects
// edits into local form state; "Save" issues `useUpdateSettingsMutation`
// and the response writes through to the cache. Reset re-primes the form
// from the latest server snapshot. There's no WS-driven freshness:
// settings only matter on the *next* codex spawn, so a stale form would
// just produce a slightly out-of-date defaults — the user always edits
// against what they last fetched.

import { useEffect } from 'react';
import { useState } from '../shared/state';
import { Crumbs } from '../shared/components/Crumbs';
import { useSettingsQuery, useUpdateSettingsMutation } from '../api/queries';
import { useTheme, type ThemeMode } from '../app/theme';
import type { Route } from '../types';

type FormState = {
  http_proxy: string;
  https_proxy: string;
};

function emptyForm(): FormState {
  return { http_proxy: '', https_proxy: '' };
}

function fromBag(bag?: { settings: Record<string, string> }): FormState {
  const s = bag?.settings ?? {};
  return {
    http_proxy: s.http_proxy ?? '',
    https_proxy: s.https_proxy ?? '',
  };
}

export function SettingsPage({ onGo }: { onGo: (r: Route) => void }) {
  const settingsQ = useSettingsQuery();
  const updateSettings = useUpdateSettingsMutation();

  const [form, setForm] = useState<FormState>(emptyForm);
  // Note: query loads + the form starts empty. Sync the form down whenever
  // the server snapshot changes (initial fetch, post-save echo). We treat
  // the server bag as authoritative — there's no offline editing concern
  // here because there's only one user and one source of truth.
  useEffect(() => {
    if (settingsQ.data) setForm(fromBag(settingsQ.data));
  }, [settingsQ.data]);

  const [toast, setToast] = useState<string | null>(null);
  useEffect(() => {
    if (!toast) return;
    const id = window.setTimeout(() => setToast(null), 2500);
    return () => window.clearTimeout(id);
  }, [toast]);

  const dirty =
    settingsQ.data !== undefined &&
    (form.http_proxy !== (settingsQ.data.settings.http_proxy ?? '') ||
      form.https_proxy !== (settingsQ.data.settings.https_proxy ?? ''));

  const onSave = async () => {
    // Send both keys every time — the route maps empty strings to a
    // delete server-side, so unsetting a field this way is explicit.
    try {
      await updateSettings.mutateAsync({
        settings: {
          http_proxy: form.http_proxy.trim() || null,
          https_proxy: form.https_proxy.trim() || null,
        },
      });
      setToast('Saved.');
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      setToast(`Save failed: ${msg}`);
    }
  };

  const onReset = () => {
    setForm(fromBag(settingsQ.data));
  };

  const saving = updateSettings.isPending;

  return (
    <div className="col wide">
      <Crumbs items={[
        { label: 'Today', onClick: () => onGo({ name: 'today' }) },
        { label: 'Settings' },
      ]} />

      <div className="page-head" style={{ marginBottom: 24 }}>
        <h1 style={{ margin: 0 }}>Settings</h1>
        <p className="synth" style={{ marginTop: 6 }}>
          App-global preferences. Changes apply to new spawns; running cards keep their startup snapshot.
        </p>
      </div>

      <section className="settings-section">
        <h2 className="settings-section-title">Network</h2>
        <p className="settings-section-hint">
          HTTP / HTTPS proxy used when launching new codex cards. Leave empty
          to inherit the container's defaults (e.g.
          <code> http://127.0.0.1:10809</code>).
        </p>

        <form
          className="schema-form"
          onSubmit={(e) => {
            e.preventDefault();
            if (!dirty || saving) return;
            void onSave();
          }}
        >
          <label htmlFor="settings-http-proxy" className="schema-form-field">
            <span className="schema-form-label">HTTP proxy</span>
            <input
              id="settings-http-proxy"
              type="text"
              className="schema-form-input"
              value={form.http_proxy}
              placeholder="http://127.0.0.1:10809"
              onChange={(e) =>
                setForm((f) => ({ ...f, http_proxy: e.target.value }))
              }
              spellCheck={false}
              autoCapitalize="off"
              autoCorrect="off"
            />
            <span className="settings-field-hint">
              Defaults to container env (e.g. <code>http://127.0.0.1:10809</code>) when empty.
            </span>
          </label>

          <label htmlFor="settings-https-proxy" className="schema-form-field">
            <span className="schema-form-label">HTTPS proxy</span>
            <input
              id="settings-https-proxy"
              type="text"
              className="schema-form-input"
              value={form.https_proxy}
              placeholder="http://127.0.0.1:10809"
              onChange={(e) =>
                setForm((f) => ({ ...f, https_proxy: e.target.value }))
              }
              spellCheck={false}
              autoCapitalize="off"
              autoCorrect="off"
            />
            <span className="settings-field-hint">
              Defaults to container env (e.g. <code>http://127.0.0.1:10809</code>) when empty.
            </span>
          </label>

          <div className="schema-form-actions">
            <button
              type="button"
              className="schema-form-cancel"
              onClick={onReset}
              disabled={!dirty || saving}
            >
              Reset
            </button>
            <button
              type="submit"
              className="schema-form-submit"
              disabled={!dirty || saving}
            >
              {saving ? 'Saving…' : 'Save'}
            </button>
          </div>
        </form>

        {settingsQ.error && (
          <p className="settings-error">
            Failed to load settings: {settingsQ.error.message}
          </p>
        )}

        {toast && <div className="settings-toast">{toast}</div>}
      </section>

      <AppearanceSection />
    </div>
  );
}

// ---------- Appearance ----------
//
// Light / Dark / System radio group. Backed by `useTheme()` from
// `app/theme.tsx`; no server round-trip — the preference lives in
// localStorage (`calm.theme`) and is read synchronously on boot by
// the provider. The TitleBar toggle button writes the same store
// (explicit 'light' / 'dark'), so toggling there flips this radio
// in real time without any extra wiring.
function AppearanceSection() {
  const { mode, setMode } = useTheme();
  const options: { value: ThemeMode; label: string; hint: string }[] = [
    { value: 'light', label: 'Light', hint: 'Always light' },
    { value: 'dark', label: 'Dark', hint: 'Always dark' },
    { value: 'system', label: 'System', hint: 'Follow your OS' },
  ];
  return (
    <section className="settings-section" aria-labelledby="settings-appearance-title">
      <h2 className="settings-section-title" id="settings-appearance-title">
        Appearance
      </h2>
      <p className="settings-section-hint">
        Choose how the app picks its color theme. The TitleBar toggle button
        cycles between Light and Dark; pick System here to track your OS
        preference instead.
      </p>
      <fieldset
        className="schema-form"
        style={{ border: 'none', padding: 0, margin: 0, gap: 6 }}
      >
        <legend
          className="schema-form-label"
          // Visually hidden — the section h2 already labels this fieldset
          // via aria-labelledby. The <legend> still helps assistive tech
          // identify the grouping when navigating by form control.
          style={{
            position: 'absolute',
            width: 1,
            height: 1,
            overflow: 'hidden',
            clip: 'rect(0 0 0 0)',
            whiteSpace: 'nowrap',
          }}
        >
          Theme
        </legend>
        {options.map((opt) => (
          <label
            key={opt.value}
            htmlFor={`settings-theme-${opt.value}`}
            className="schema-form-field"
            style={{ flexDirection: 'row', alignItems: 'center', gap: 10 }}
          >
            <input
              id={`settings-theme-${opt.value}`}
              type="radio"
              name="settings-theme"
              value={opt.value}
              checked={mode === opt.value}
              onChange={() => setMode(opt.value)}
            />
            <span className="schema-form-label" style={{ marginBottom: 0 }}>
              {opt.label}
            </span>
            <span className="settings-field-hint" style={{ marginTop: 0 }}>
              {opt.hint}
            </span>
          </label>
        ))}
      </fieldset>
    </section>
  );
}
