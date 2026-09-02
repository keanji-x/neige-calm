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

import { Heading as AstryxHeading } from '@astryxdesign/core/Heading';
import { List as AstryxList, ListItem as AstryxListItem } from '@astryxdesign/core/List';
import { Selector as AstryxSelector } from '@astryxdesign/core/Selector';
import { SideNav as AstryxSideNav, SideNavItem as AstryxSideNavItem } from '@astryxdesign/core/SideNav';
import { Text as AstryxText } from '@astryxdesign/core/Text';
import { TextInput as AstryxTextInput } from '@astryxdesign/core/TextInput';
import { VisuallyHidden as AstryxVisuallyHidden } from '@astryxdesign/core/VisuallyHidden';
import { useEffect, useRef, type ReactNode } from 'react';

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
  /**
   * Commits one key. The returned promise **is** the row's status: this pane
   * follows it per field, so the confirmation and the failure belong to the
   * request they came from.
   */
  onSave: (patch: SettingsPatch) => void | Promise<void>;
  onRetryLoad: () => void;
  /** Tests shorten the confirmation window; production uses the default. */
  savedNoticeMs?: number;
}>;

type Draft = { http: string; https: string };

/** Which field a commit was for. */
type ProxyField = 'http' | 'https';

const PROXY_FIELDS = Object.freeze(['http', 'https'] as const);

const PROXY_KEY_OF: Readonly<Record<ProxyField, string>> = Object.freeze({
  http: HTTP_PROXY_KEY,
  https: HTTPS_PROXY_KEY,
});

const PROXY_LABEL_OF: Readonly<Record<ProxyField, string>> = Object.freeze({
  http: 'HTTP proxy',
  https: 'HTTPS proxy',
});

/**
 * What one row's last commit is doing.
 *
 * Per field, and derived from that field's own promise — **not** from a
 * pane-level `saving` / `saveError` / `savedAt` triple with a single "which row
 * was it" pointer beside it. That shape was wrong in three measurable ways, all
 * reproduced before this was written:
 *
 *   * commit HTTP, then commit HTTPS, then HTTP's request fails ⇒ the failure
 *     painted on the **HTTPS** row, and HTTP's failure was never shown at all,
 *     so the reader left believing a proxy was saved that was not;
 *   * a still-unretired confirmation from HTTP's save turned into a green tick
 *     on HTTPS the moment HTTPS was committed — before its request resolved;
 *   * `saving` cleared when the first of two flights settled, so the busy
 *     marker lied about the other.
 *
 * `seq` is what makes a stale response harmless: a second commit on the same
 * field bumps it, and a response whose sequence is no longer current is
 * dropped rather than allowed to overwrite the newer one's outcome.
 */
type RowStatus =
  | Readonly<{ phase: 'idle' }>
  | Readonly<{ phase: 'saving'; value: string }>
  | Readonly<{ phase: 'saved'; at: number; value: string }>
  | Readonly<{ phase: 'failed'; message: string; value: string }>;

const IDLE: RowStatus = Object.freeze({ phase: 'idle' });

function useRetiringNotice(
  field: ProxyField,
  row: RowStatus,
  setStatus: (update: (current: Readonly<Record<ProxyField, RowStatus>>) => Readonly<Record<ProxyField, RowStatus>>) => void,
  savedNoticeMs: number,
): void {
  const savedAt = row.phase === 'saved' ? row.at : null;
  useEffect(() => {
    if (savedAt === null) return;
    const id = setTimeout(() => {
      setStatus((current) => (current[field].phase === 'saved'
        ? { ...current, [field]: IDLE }
        : current));
    }, savedNoticeMs);
    return () => clearTimeout(id);
  }, [field, savedAt, savedNoticeMs, setStatus]);
}

export function NetworkPane({
  settings, loadError, onSave, onRetryLoad, savedNoticeMs = SAVED_NOTICE_MS,
}: NetworkPaneProps) {
  const loaded = settings !== undefined;
  const incoming: Draft = {
    http: settings?.[HTTP_PROXY_KEY] ?? '',
    https: settings?.[HTTPS_PROXY_KEY] ?? '',
  };

  // Seeding compares by *value*, not by object identity: a parent that hands
  // back a fresh object on every render (a query cache does) must not wipe out
  // what the reader is typing. A genuine server change does re-seed.
  const [seed, setSeed] = useState<Draft | null>(null);
  const [draft, setDraft] = useState<Draft>({ http: '', https: '' });
  /**
   * What this pane last *told* the server for each field, or `null` when it has
   * told it nothing since the bag it is holding.
   *
   * The commit guard cannot compare against `base` alone: `base` only moves
   * when the server's bag comes back, so between a commit and its echo the same
   * value looked "changed" and was sent again — Enter followed by Tab wrote
   * twice, and closing the dialog on an in-flight commit re-sent it. Cleared
   * whenever a genuinely new bag arrives, at which point `base` is the truth
   * again.
   */
  const sent = useRef<Record<ProxyField, string | null>>({ http: null, https: null });

  if (loaded && (seed === null || seed.http !== incoming.http || seed.https !== incoming.https)) {
    const previous = seed;
    setSeed(incoming);
    /* Only where the bag *confirms* what we sent. Clearing both entries on any
       new bag dropped the reference for a field whose write was still in
       flight — after which the next blur, or the close cleanup, sent the same
       value again. */
    for (const field of PROXY_FIELDS) {
      if (sent.current[field] === incoming[field]) sent.current[field] = null;
    }
    setDraft((current) => ({
      http: previous === null || current.http === previous.http ? incoming.http : current.http,
      https: previous === null || current.https === previous.https ? incoming.https : current.https,
    }));
  }

  const base = seed ?? { http: '', https: '' };
  const [status, setStatus] = useState<Readonly<Record<ProxyField, RowStatus>>>(
    { http: IDLE, https: IDLE },
  );
  const sequence = useRef<Record<ProxyField, number>>({ http: 0, https: 0 });
  const referenceFor = (field: ProxyField) => sent.current[field] ?? base[field];

  /**
   * Commit on **blur and Enter, never per keystroke.**
   *
   * There is no Save button: a proxy is one value, and a settings screen that
   * asks you to press Save for one value is asking you to do the app's
   * bookkeeping. But a half-typed URL is not a value — saving per keystroke
   * would PUT `h`, `ht`, `htt`… and leave whatever the reader stopped at as the
   * workspace's proxy if they walked away mid-word. Leaving the field is the
   * moment the value is finished, and Enter is the same intent stated
   * explicitly.
   *
   * A value equal to the last one the server gave us commits nothing: focusing
   * and leaving a field the reader never edited must not write.
   */
  const commit = (field: ProxyField, value: string) => {
    if (value === referenceFor(field)) return;
    sent.current[field] = value;
    const ticket = (sequence.current[field] += 1);
    const settle = (next: RowStatus) => {
      // A response for a superseded commit says nothing about the current one.
      if (sequence.current[field] !== ticket) return;
      setStatus((current) => ({ ...current, [field]: next }));
    };
    setStatus((current) => ({ ...current, [field]: { phase: 'saving', value } }));
    void Promise.resolve(onSave({ [PROXY_KEY_OF[field]]: value === '' ? null : value }))
      .then(() => { settle({ phase: 'saved', at: Date.now(), value }); })
      .catch((error: unknown) => {
        /* The reference goes back to the server's bag: `sent` records what the
           server was told and *took*. Leaving a failed value in it made the
           obvious retry — refocus, Enter — a no-op, so a failed save could not
           be retried at all and the row went quiet on the next keystroke. */
        sent.current[field] = null;
        settle({ phase: 'failed', message: error instanceof Error ? error.message : 'Save failed.', value });
      });
  };

  /*
   * Closing the dialog commits what is in the fields.
   *
   * Escape and a backdrop click unmount the focused input, and removing a
   * focused element fires **no** blur — so without this the reader's typing
   * left with the dialog, silently, on a screen whose whole premise is that it
   * saves itself. The refs are what make it correct at unmount time: the
   * cleanup runs once, after the last render, and must read the values from
   * then rather than the ones captured when the effect was created.
   */
  const pending = useRef({ draft, base, onSave });
  pending.current = { draft, base, onSave };
  useEffect(() => () => {
    const { draft: last, base: seeded, onSave: save } = pending.current;
    for (const field of PROXY_FIELDS) {
      // The same guard the blur path uses: what was already sent is not resent
      // just because the bag has not echoed it back yet.
      if (last[field] === (sent.current[field] ?? seeded[field])) continue;
      const value = last[field];
      void Promise.resolve(save({ [PROXY_KEY_OF[field]]: value === '' ? null : value })).catch(() => {
        // Nothing is mounted to report to. The write still went out; the next
        // visit re-reads the bag and shows whatever actually landed.
      });
    }
  }, []);

  /*
   * Per row, and keyed on *that row's* saved timestamp.
   *
   * One effect over the whole `status` object restarted the single shared timer
   * every time the other row changed, so HTTPS going busy could hold HTTP's
   * tick on screen for far longer than the notice window.
   */
  useRetiringNotice('http', status.http, setStatus, savedNoticeMs);
  useRetiringNotice('https', status.https, setStatus, savedNoticeMs);

  /**
   * The confirmation is the **tick and nothing else**.
   *
   * "Saved." beside a green tick is the tick said twice, and the word costs the
   * row a line that then reflows the rows under it every time you leave a
   * field. A failure keeps its sentence, because "something went wrong" is not
   * a thing a mark can say.
   *
   * The word does not disappear for a screen reader: it goes to the
   * always-mounted live region beside the field. Always-mounted matters —
   * screen readers commonly do not announce a region that arrives in the same
   * mutation as its text, so the region has to exist before it has something
   * to say.
   */
  const statusFor = (field: ProxyField) => {
    const row = status[field];
    if (row.phase === 'idle') return undefined;
    /* A verdict describes **the value it was for**. Once the reader has moved
       the field on, neither the tick nor the error is about what is on screen:
       a response that settled while they were typing the next value used to
       paint a tick — and announce "Saved." — beside a value that was never
       sent. */
    if (row.value !== draft[field]) return undefined;
    if (row.phase === 'failed') return { type: 'error' as const, message: row.message };
    if (row.phase === 'saved') return { type: 'success' as const };
    return undefined;
  };

  const proxyRow = (field: ProxyField) => (
    <SettingRow
      key={field}
      title={PROXY_LABEL_OF[field]}
      description="Empty inherits the container's own proxy."
      control={(
        <>
          <AstryxVisuallyHidden role="status">
            {statusFor(field)?.type === 'success' ? 'Saved.' : ''}
          </AstryxVisuallyHidden>
          <AstryxTextInput
            label={PROXY_LABEL_OF[field]}
            isLabelHidden
            value={draft[field]}
            placeholder="http://127.0.0.1:10809"
            status={statusFor(field)}
            onChange={(value) => setDraft({ ...draft, [field]: value })}
            onBlur={() => commit(field, draft[field])}
            onKeyDown={(event) => { if (event.key === 'Enter') commit(field, draft[field]); }}
            width={CONTROL_WIDTH}
            /* The field stays editable while its write is in flight: a proxy
               save is one request, and blocking the field would drop the next
               keystroke. */
            data-nc-state={status[field].phase === 'saving' ? 'busy' : undefined}
          />
        </>
      )}
    />
  );

  return (
    <SettingsPane
      title="Network"
      lede="Proxies used when launching new agent cards. Changes save when you leave the field; running cards keep the proxy they started with."
    >
      {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
      {/* INV-SETTINGS-002 — a loading line, never an empty field: an empty form
          would let the reader save blanks over real values. */}
      {!loaded && loadError === null && <AstryxText as="p" color="secondary">Loading settings…</AstryxText>}
      {loaded && <SettingsList>{PROXY_FIELDS.map((field) => proxyRow(field))}</SettingsList>}
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
