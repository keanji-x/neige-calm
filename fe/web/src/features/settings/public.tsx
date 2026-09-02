// Settings — workspace preferences. Presentational and props-driven: it never
// calls an API. Loading, saving and error state all arrive as props, and the
// patch it builds leaves through `onSave` (features must not import app).
//
// ## Why an overlay with a nav column
//
// Settings used to be a page with four stacked cards, and #1230 added a fifth
// group (Templates) as a drill-in row. Stacking is what a settings screen does
// until it has more than one group of groups: Network is one line of the same
// thought as Appearance, Templates and Plugins are each their own screen with
// their own reads and their own failure. A column that names the groups says
// that in the layout instead of making the reader scroll to discover it.
//
// It renders inside a dialog (`ui/dialog`, supplied by the app layer) rather
// than as a full page for the reason the reader keeps stating by pressing
// Escape: Settings is somewhere you *step into* from wherever you were, and the
// way out should be the same one gesture everywhere in the app. The sections
// stay real routes underneath — `SettingsSurface` is chrome, not state, so Back
// still leaves the template editor rather than leaving Settings.

import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { Divider as AstryxDivider } from '@astryxdesign/core/Divider';
import { Field as AstryxField } from '@astryxdesign/core/Field';
import { FormLayout as AstryxFormLayout } from '@astryxdesign/core/FormLayout';
import { Heading as AstryxHeading } from '@astryxdesign/core/Heading';
import {
  MetadataList as AstryxMetadataList,
  MetadataListItem as AstryxMetadataListItem,
} from '@astryxdesign/core/MetadataList';
import { SideNav as AstryxSideNav, SideNavItem as AstryxSideNavItem } from '@astryxdesign/core/SideNav';
import { Text as AstryxText } from '@astryxdesign/core/Text';
import {
  SegmentedControl as AstryxSegmentedControl,
  SegmentedControlItem as AstryxSegmentedControlItem,
} from '@astryxdesign/core/SegmentedControl';
import { TextInput as AstryxTextInput } from '@astryxdesign/core/TextInput';
import { useEffect, type ReactNode } from 'react';

import { HTTPS_PROXY_KEY, HTTP_PROXY_KEY, type SettingsPatch } from '../../../../core/domain/settings.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { useState } from '../../ui/state/public.ts';
import styles from './settings.module.css';

/**
 * The groups the nav column lists, in the order it lists them.
 *
 * `general` first because it is the only one that is settings in the narrow
 * sense — a form you fill in. The other two are places, and a place belongs
 * below the thing you came here to change.
 */
export type SettingsSection = 'general' | 'templates' | 'plugins';

const SETTINGS_SECTIONS = Object.freeze([
  Object.freeze({ id: 'general', label: 'General', icon: 'settings' }),
  Object.freeze({ id: 'templates', label: 'Templates', icon: 'file-text' }),
  Object.freeze({ id: 'plugins', label: 'Plugins', icon: 'puzzle' }),
] as const);

export type SettingsSurfaceProps = Readonly<{
  section: SettingsSection;
  onSelectSection: (section: SettingsSection) => void;
  children: ReactNode;
}>;

/**
 * The two-column frame every Settings route renders inside: a nav column that
 * names the groups, and the pane for the current one.
 *
 * ## Built from `SideNav`, not from hand-written rows
 *
 * The first cut hand-rolled the column — a `<nav>` of `<button>`s with a
 * CSS-module pill for the current one — and it was wrong twice over. Astryx
 * ships this exact component (`SideNav` / `SideNavItem isSelected`), so the
 * hand-rolled version was a second, worse copy of a solved problem: it had no
 * icons, its own padding scale, and its selected pill hung outside the column
 * on a negative margin, where the dialog body's `overflow: auto` clipped the
 * left half of it off.
 *
 * `SideNavItem` renders a `<button>` when given `onClick` and no `href`, which
 * keeps INV-A11Y-061 (navigation is a button plus a callback, never an
 * `<a href>`); `aria-current="page"` is still stamped here, because
 * `isSelected` is a visual state and the current *route* is a fact a screen
 * reader has to be told.
 *
 * The dialog above supplies the title and the `×`; this component deliberately
 * has neither, so there is exactly one close affordance on screen.
 */
export function SettingsSurface({ section, onSelectSection, children }: SettingsSurfaceProps) {
  return (
    <div className={styles.surface}>
      <AstryxSideNav aria-label="Settings sections" xstyle={undefined} className={styles.sectionNav}>
        {SETTINGS_SECTIONS.map((entry) => (
          <AstryxSideNavItem
            key={entry.id}
            label={entry.label}
            icon={entry.icon}
            isSelected={entry.id === section}
            aria-current={entry.id === section ? 'page' : undefined}
            onClick={() => onSelectSection(entry.id)}
          />
        ))}
      </AstryxSideNav>
      <div className={styles.pane}>{children}</div>
    </div>
  );
}

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
  themeMode: ThemeMode;
  onThemeModeChange: (mode: ThemeMode) => void;
  /** Tests shorten the confirmation window; production uses the default. */
  savedNoticeMs?: number;
}>;

const THEME_MODES = Object.freeze(['light', 'dark', 'system'] as const);
const SAVED_NOTICE_MS = 4000;
const THEME_CONTROL_ID = 'nc-settings-theme';

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
  settings, loadError, saving, saveError, savedAt, onSave, onRetryLoad,
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
    <div className={styles.paneBody}>
      <div className={styles.form}>
        {/*
          * Groups are a heading, a divider and rows — not bordered cards.
          *
          * Cards were the previous shape and they said the wrong thing: a card
          * is a boundary, and Network / Appearance / About are three parts of
          * one screen, not three objects. Astryx's own settings guidance is
          * this shape ("use horizontal-labels for settings pages where labels
          * sit beside their inputs"), and it is what a settings pane looks like
          * everywhere the reader already knows one from: a label column on the
          * left, its control on the right, hairlines between groups.
          */}
        <section className={styles.group} aria-labelledby="nc-settings-network">
          <AstryxHeading level={3} id="nc-settings-network">Network</AstryxHeading>
          <AstryxText as="p" color="secondary">
            Used when launching new agent cards. Empty inherits the container&rsquo;s own proxy.
          </AstryxText>
          {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
          {!loaded && loadError === null
            ? <AstryxText as="p" color="secondary">Loading settings…</AstryxText>
            : loaded ? (
              <>
                <AstryxFormLayout direction="horizontal-labels">
                  <AstryxTextInput
                    label="HTTP proxy"
                    value={draft.http}
                    placeholder="http://127.0.0.1:10809"
                    onChange={(value) => setDraft({ ...draft, http: value })}
                    size="lg"
                    width="100%"
                  />
                  <AstryxTextInput
                    label="HTTPS proxy"
                    value={draft.https}
                    placeholder="http://127.0.0.1:10809"
                    onChange={(value) => setDraft({ ...draft, https: value })}
                    size="lg"
                    width="100%"
                  />
                </AstryxFormLayout>
                {/* The actions sit at the end of the block they commit, which
                    is the convention this layout comes with — not floating at
                    the bottom of a pane that holds three unrelated groups. */}
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

        <AstryxDivider />

        <section className={styles.group} aria-labelledby="nc-settings-appearance">
          <AstryxHeading level={3} id="nc-settings-appearance">Appearance</AstryxHeading>
          <AstryxFormLayout direction="horizontal-labels">
            {/* `SegmentedControl`'s own `label` is `aria-label` only — it is
                never rendered — so on its own the control would sit in the
                grid's input column with an empty label column beside it.
                `Field` is astryx's stated wrapper for exactly that case
                ("for controls that do not already provide field under the
                hood"), and it is what puts `Theme` in the label column with
                the proxy rows above. */}
            <AstryxField label="Theme" inputID={THEME_CONTROL_ID}>
              <AstryxSegmentedControl
                id={THEME_CONTROL_ID}
                className={styles.themeControl}
                value={themeMode}
                onChange={(value) => onThemeModeChange(value === 'light' || value === 'dark' ? value : 'system')}
                /* Still passed: this is the radiogroup's `aria-label`, and it
                   is what names the group for a screen reader. `Field`'s label
                   is the visible one. */
                label="Theme"
                size="lg"
              >
                {THEME_MODES.map((mode) => (
                  <AstryxSegmentedControlItem key={mode} value={mode} label={themeLabel(mode)} />
                ))}
              </AstryxSegmentedControl>
            </AstryxField>
          </AstryxFormLayout>
          <AstryxText as="p" color="secondary">Stored on this device.</AstryxText>
        </section>

        <AstryxDivider />

        <section className={styles.group} aria-labelledby="nc-settings-about">
          <AstryxHeading level={3} id="nc-settings-about">About</AstryxHeading>
          <AstryxMetadataList columns="single" label={{ position: 'start', width: '6.5rem' }}>
            <AstryxMetadataListItem label="Version">
              <span className={styles.aboutValue}>{__NC_VERSION__}</span>
            </AstryxMetadataListItem>
            <AstryxMetadataListItem label="Build">
              <span className={styles.aboutValue}>{__NC_BUILD__}</span>
            </AstryxMetadataListItem>
          </AstryxMetadataList>
        </section>
      </div>
    </div>
  );
}
