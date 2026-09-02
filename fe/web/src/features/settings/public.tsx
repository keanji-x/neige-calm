// Settings — workspace preferences, and the one row grammar every pane uses.
//
// Presentational and props-driven throughout: nothing here calls an API.
// Loading, saving and error state all arrive as props, and the patch a pane
// builds leaves through `onSave` (features must not import app).
//
// ## The standard this file defines
//
// One nav column, one pane per group, and **one row shape**. A row is:
//
//     title                                                    [ control ]
//     one sentence
//
// left-aligned text, right-aligned control, both flush with the pane's edges, a
// hairline between rows and nothing else. That is `SettingRow`, and it is the
// only way to put anything on a settings pane — so a text field, a dropdown, a
// toggle and a drill-in all sit between the same two edges and read as one
// screen rather than as four screens that happen to be adjacent.
//
// A row is **either** something you set (`control`) **or** somewhere you go
// (`onOpen`), never both: the type below makes the pair unrepresentable, and
// astryx's list guidance rejects an interactive control inside an interactive
// row for the same reason — two targets for one intent.
//
// ## Hierarchy
//
// Three levels, and only three: the dialog's title (`Settings`), the pane's
// heading plus its one-sentence lede, and the rows. Group headings *inside* a
// pane are gone — a pane holding three headed groups is the shape that made the
// old General pane read as a pile. Anything that wants a heading of its own is
// a section in the nav column instead, which is why Network / Appearance /
// About are now three entries rather than three stacked groups.
//
// ## Icons
//
// From astryx's built-in registry only; the app does not draw its own for this.
// That set is small and has no "network" or "appearance", so each section takes
// the nearest available sense and says why at the point of choice.

import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { Heading as AstryxHeading } from '@astryxdesign/core/Heading';
import { List as AstryxList, ListItem as AstryxListItem } from '@astryxdesign/core/List';
import { Selector as AstryxSelector } from '@astryxdesign/core/Selector';
import { SideNav as AstryxSideNav, SideNavItem as AstryxSideNavItem } from '@astryxdesign/core/SideNav';
import { Text as AstryxText } from '@astryxdesign/core/Text';
import { TextInput as AstryxTextInput } from '@astryxdesign/core/TextInput';
import { useEffect, type ReactNode } from 'react';

import { HTTPS_PROXY_KEY, HTTP_PROXY_KEY, type SettingsPatch } from '../../../../core/domain/settings.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { useState } from '../../ui/state/public.ts';
import styles from './settings.module.css';

/**
 * The groups the nav column lists, in the order it lists them.
 *
 * Ordered by how often a reader comes for them, with `about` last because it is
 * the one group you read rather than change. `network` is first and is what
 * `/settings` resolves to, so the bare route lands on a real group rather than
 * on a container page.
 */
export type SettingsSection = 'network' | 'appearance' | 'templates' | 'plugins' | 'about';

/*
 * Icon names are astryx's built-in semantic set, which has 26 entries and none
 * of them called "network" or "appearance". Rather than draw four one-off
 * glyphs, each section takes the nearest available sense:
 *
 *   externalLink — traffic leaving this machine, which is all Network is about
 *   viewColumns  — how the app is laid out and painted
 *   copy         — a template is the thing that gets copied into a new wave
 *   wrench       — the workspace's tooling
 *   info         — read-only facts about the build
 */
const SETTINGS_SECTIONS = Object.freeze([
  Object.freeze({ id: 'network', label: 'Network', icon: 'externalLink' }),
  Object.freeze({ id: 'appearance', label: 'Appearance', icon: 'viewColumns' }),
  Object.freeze({ id: 'templates', label: 'Templates', icon: 'copy' }),
  Object.freeze({ id: 'plugins', label: 'Plugins', icon: 'wrench' }),
  Object.freeze({ id: 'about', label: 'About', icon: 'info' }),
] as const);

export type SettingsSurfaceProps = Readonly<{
  section: SettingsSection;
  onSelectSection: (section: SettingsSection) => void;
  children: ReactNode;
}>;

/**
 * The two-column frame every Settings route renders inside.
 *
 * Built from astryx's `SideNav` / `SideNavItem isSelected` rather than
 * hand-written rows: the design system ships this component, and the
 * hand-rolled version was a worse copy of it whose selected pill hung outside
 * the column on a negative margin, where the dialog body's `overflow: auto`
 * clipped half of it away.
 *
 * `SideNavItem` renders a `<button>` for an `onClick` without an `href`, which
 * keeps INV-A11Y-061; `aria-current="page"` is stamped on top, because
 * `isSelected` is a visual state and the current *route* is a fact a screen
 * reader has to be told.
 *
 * The dialog above supplies the title and the `×`, so this has neither — there
 * is exactly one close affordance on screen.
 */
export function SettingsSurface({ section, onSelectSection, children }: SettingsSurfaceProps) {
  return (
    <div className={styles.surface}>
      <AstryxSideNav aria-label="Settings sections" className={styles.sectionNav}>
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
 * A pane: its heading, one sentence saying what the group is for, and its rows.
 *
 * The lede is required. A settings group that cannot be described in one
 * sentence is two groups, and the nav column is where the second one goes.
 */
export function SettingsPane({ title, lede, children }: Readonly<{
  title: string;
  lede: string;
  children: ReactNode;
}>) {
  const headingId = `nc-settings-${title.toLowerCase().replace(/\s+/g, '-')}`;
  return (
    <div className={styles.paneBody}>
      <section className={styles.group} aria-labelledby={headingId}>
        <AstryxHeading level={3} id={headingId}>{title}</AstryxHeading>
        <AstryxText as="p" color="secondary">{lede}</AstryxText>
        {children}
      </section>
    </div>
  );
}

/** The rows of a pane. Hairlines between them and nothing else. */
export function SettingsList({ children }: Readonly<{ children: ReactNode }>) {
  return <AstryxList hasDividers density="balanced" className={styles.list}>{children}</AstryxList>;
}

/**
 * One row.
 *
 * `control` and `onOpen` are mutually exclusive *by type*: a row you set and a
 * row you walk into are different things, and a row that is both is two click
 * targets for one intent.
 */
export type SettingRowProps = Readonly<{
  title: string;
  /** One sentence. Omitted when the title already says everything. */
  description?: ReactNode;
  /** A badge or status mark before the title. */
  startContent?: ReactNode;
}> & (
  | Readonly<{ control: ReactNode; onOpen?: never }>
  | Readonly<{ onOpen: () => void; control?: never }>
);

export function SettingRow({ title, description, startContent, control, onOpen }: SettingRowProps) {
  return (
    <AstryxListItem
      className={styles.row}
      label={title}
      description={description}
      startContent={startContent}
      /* A drill-in row ends in a chevron and the whole row is the target; a
         setting row ends in its own control and the row is not clickable. */
      endContent={onOpen === undefined ? control : <Icon name="chevron-right" />}
      onClick={onOpen}
    />
  );
}

/**
 * Mirrors `app/theme`'s mode union by value. `features/**` must not import
 * `app/**`, so the union is declared here and the app layer adapts to it; the
 * two are kept in step by the router wiring, not by a type import.
 */
export type ThemeMode = 'light' | 'dark' | 'system';

const THEME_OPTIONS = Object.freeze([
  Object.freeze({ value: 'light', label: 'Light' }),
  Object.freeze({ value: 'dark', label: 'Dark' }),
  Object.freeze({ value: 'system', label: 'System' }),
] as const);

const SAVED_NOTICE_MS = 4000;

/** Every right-hand control is this wide, so the pane has one trailing edge. */
const CONTROL_WIDTH = 260;

// ---------------------------------------------------------------------------
// Network
// ---------------------------------------------------------------------------

export type NetworkPaneProps = Readonly<{
  /** `undefined` means "still loading" — never render an empty form for it. */
  settings: Readonly<Record<string, string>> | undefined;
  loadError: string | null;
  saving: boolean;
  saveError: string | null;
  /** Timestamp of the last successful save; drives the transient confirmation. */
  savedAt: number | null;
  onSave: (patch: SettingsPatch) => void | Promise<void>;
  onRetryLoad: () => void;
  /** Tests shorten the confirmation window; production uses the default. */
  savedNoticeMs?: number;
}>;

type Draft = { http: string; https: string };

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

export function NetworkPane({
  settings, loadError, saving, saveError, savedAt, onSave, onRetryLoad,
  savedNoticeMs = SAVED_NOTICE_MS,
}: NetworkPaneProps) {
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
    <SettingsPane
      title="Network"
      lede="Proxies used when launching new agent cards. Running cards keep the proxy they started with."
    >
      {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
      {/* INV-SETTINGS-002 — a loading line, never an empty field: an empty form
          would let the reader save blanks over real values. */}
      {!loaded && loadError === null && <AstryxText as="p" color="secondary">Loading settings…</AstryxText>}
      {loaded && (
        <>
          <SettingsList>
            <SettingRow
              title="HTTP proxy"
              description="Empty inherits the container's own proxy."
              control={(
                <AstryxTextInput
                  label="HTTP proxy"
                  isLabelHidden
                  value={draft.http}
                  placeholder="http://127.0.0.1:10809"
                  onChange={(value) => setDraft({ ...draft, http: value })}
                  width={CONTROL_WIDTH}
                />
              )}
            />
            <SettingRow
              title="HTTPS proxy"
              description="Empty inherits the container's own proxy."
              control={(
                <AstryxTextInput
                  label="HTTPS proxy"
                  isLabelHidden
                  value={draft.https}
                  placeholder="http://127.0.0.1:10809"
                  onChange={(value) => setDraft({ ...draft, https: value })}
                  width={CONTROL_WIDTH}
                />
              )}
            />
          </SettingsList>
          {/* The actions close the block they commit, at its trailing edge —
              the same edge every control on the pane ends at. */}
          <div className={styles.actions}>
            <AstryxButton
              label={saving ? 'Saving…' : 'Save'}
              variant="primary"
              isDisabled={!dirty && !saving}
              isLoading={saving}
              isInterruptible
              data-nc-state={saving ? 'busy' : undefined}
              onClick={() => { if (saving) return; void onSave(buildPatch(draft, base)); }}
            />
            <AstryxButton
              label="Reset"
              variant="secondary"
              isDisabled={!dirty || saving}
              data-nc-state={saving ? 'busy' : undefined}
              onClick={() => { if (saving) return; setDraft(base); }}
            />
            {/*
              * `data-nc-settings-saved` is the e2e seam, not decoration.
              * `role="status"` cannot locate this span: every Astryx `Button`
              * renders its own unconditional, empty-text `role="status"` live
              * region for loading announcements, so the two buttons beside
              * this one make `getByRole('status')` resolve to three elements.
              * Filtering those by the text we are about to assert would be
              * circular — it could only prove "some status says Saved.", never
              * "the save succeeded".
              */}
            {showSaved && (
              <span className={styles.saved} role="status" data-nc-settings-saved>Saved.</span>
            )}
          </div>
          {saveError !== null && <p className={styles.error} role="alert">{saveError}</p>}
        </>
      )}
    </SettingsPane>
  );
}

// ---------------------------------------------------------------------------
// Appearance
// ---------------------------------------------------------------------------

export function AppearancePane({ themeMode, onThemeModeChange }: Readonly<{
  themeMode: ThemeMode;
  onThemeModeChange: (mode: ThemeMode) => void;
}>) {
  return (
    <SettingsPane title="Appearance" lede="How this device paints the app. Not shared with your other devices.">
      <SettingsList>
        <SettingRow
          title="Theme"
          description="System follows your operating system's setting."
          control={(
            /* A dropdown, not three segments. Three fixed segments spend the
               row's whole trailing edge showing two options nobody picked, and
               they cannot grow — a fourth theme would have to change the
               control. A `Selector` states the current value and keeps the
               alternatives until asked, which is what the rest of the rows on
               this screen do too. */
            <AstryxSelector
              label="Theme"
              isLabelHidden
              value={themeMode}
              options={[...THEME_OPTIONS]}
              onChange={(value) => onThemeModeChange(asThemeMode(value))}
              width={CONTROL_WIDTH}
            />
          )}
        />
      </SettingsList>
    </SettingsPane>
  );
}

function asThemeMode(value: string): ThemeMode {
  return value === 'light' || value === 'dark' ? value : 'system';
}

// ---------------------------------------------------------------------------
// About
// ---------------------------------------------------------------------------

export function AboutPane() {
  return (
    <SettingsPane title="About" lede="What this build is. Read-only.">
      <SettingsList>
        <SettingRow title="Version" control={<span className={styles.aboutValue}>{__NC_VERSION__}</span>} />
        <SettingRow title="Build" control={<span className={styles.aboutValue}>{__NC_BUILD__}</span>} />
      </SettingsList>
    </SettingsPane>
  );
}
