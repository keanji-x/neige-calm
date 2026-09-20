// Settings — workspace preferences, and the one row grammar every pane uses.
// A row is either something you set (`control`) or somewhere you go (`onOpen`), never both.

import { Heading as AstryxHeading } from '@astryxdesign/core/Heading';
import { List as AstryxList, ListItem as AstryxListItem } from '@astryxdesign/core/List';
import { NumberInput as AstryxNumberInput } from '@astryxdesign/core/NumberInput';
import { Selector as AstryxSelector } from '@astryxdesign/core/Selector';
import { SideNav as AstryxSideNav, SideNavItem as AstryxSideNavItem } from '@astryxdesign/core/SideNav';
import { Text as AstryxText } from '@astryxdesign/core/Text';
import { TextInput as AstryxTextInput } from '@astryxdesign/core/TextInput';
import { VisuallyHidden as AstryxVisuallyHidden } from '@astryxdesign/core/VisuallyHidden';
import { useEffect, useRef, type ReactNode } from 'react';

import {
  HTTPS_PROXY_KEY, HTTP_PROXY_KEY, TASK_BUDGET_DEFAULT_KEY, taskBudgetDefaultFrom,
  type SettingsPatch,
} from '../../../../core/domain/settings.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { SETTINGS_SECTIONS, SettingsIndex,
  type SettingsSection, type SettingsPresentation } from './navigation.tsx';
import styles from './settings.module.css';

export type SettingsSurfaceProps = Readonly<{
  presentation: SettingsPresentation;
  section: SettingsSection;
  onSelectSection: (section: SettingsSection) => void;
  children: ReactNode;
}>;

/** The two-column frame every Settings route renders inside. `aria-current="page"` is stamped on top of `isSelected`: the current route is a fact a screen reader has to be told. */
export function SettingsSurface({ presentation, section, onSelectSection, children }: SettingsSurfaceProps) {
  return (
    <div className={`${styles.surface} ${presentation === 'desktop' ? '' : styles.mobileSurface} ${presentation === 'mobile-detail' ? styles.mobileDetail : ''}`}>
      {presentation === 'mobile-index' && <SettingsIndex onSelectSection={onSelectSection} />}
      {presentation === 'desktop' && <AstryxSideNav aria-label="Settings sections" className={styles.sectionNav}>
        {SETTINGS_SECTIONS.map((entry) => (
          <AstryxSideNavItem key={entry.id} label={entry.label} icon={entry.icon}
            isSelected={entry.id === section} aria-current={entry.id === section ? 'page' : undefined}
            onClick={() => onSelectSection(entry.id)} />
        ))}
      </AstryxSideNav>}
      <div className={styles.pane} hidden={presentation === 'mobile-index'}>{children}</div>
    </div>
  );
}

/** A pane: its heading, one sentence saying what the group is for, and its rows. */
export function SettingsPane({ title, lede, children, category }: Readonly<{
  title: string;
  /** Present only on a top-level category; drill-ins keep their own heading. */
  category?: SettingsSection;
  lede: string;
  children: ReactNode;
}>) {
  const headingId = `nc-settings-${title.toLowerCase().replace(/\s+/g, '-')}`;
  return (
    <div className={styles.paneBody}>
      <section className={styles.group} aria-labelledby={headingId}>
        <AstryxHeading level={3} id={headingId} className={category === undefined ? undefined : styles.categoryHeading}>{title}</AstryxHeading>
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

/** One row. `control` and `onOpen` are mutually exclusive by type. */
export type SettingRowProps = Readonly<{
  title: string;
  /** A quiet second mark on the title line — a version, a count, a unit. Not a place for a sentence. */
  titleSuffix?: ReactNode;
  /** One sentence. Omitted when the title already says everything. */
  description?: ReactNode;
  /** A badge or status mark before the title. */
  startContent?: ReactNode;
}> & (
  | Readonly<{ control: ReactNode; onOpen?: never }>
  | Readonly<{ onOpen: () => void; control?: never }>
);

export function SettingRow({
  title, titleSuffix, description, startContent, control, onOpen,
}: SettingRowProps) {
  return (
    <AstryxListItem
      className={`${styles.row} ${onOpen === undefined ? styles.controlRow : ''}`}
      label={titleSuffix === undefined ? title : (
        <span className={styles.rowTitle} data-nc-row-title="">
          {title}
          <span className={styles.rowTitleSuffix}>{titleSuffix}</span>
        </span>
      )}
      description={description}
      startContent={startContent}
      endContent={onOpen === undefined ? control : <Icon name="chevron-right" />}
      onClick={onOpen}
    />
  );
}

/** Mirrors `app/theme`'s mode union by value: `features/**` must not import `app/**`. */
export type ThemeMode = 'light' | 'dark' | 'system';

const THEME_OPTIONS = Object.freeze([
  Object.freeze({ value: 'light', label: 'Light' }),
  Object.freeze({ value: 'dark', label: 'Dark' }),
  Object.freeze({ value: 'system', label: 'System' }),
] as const);

const SAVED_NOTICE_MS = 4000;

/** Every right-hand control is this wide, so the pane has one trailing edge. Exported for the plugin configuration pane. */
export const CONTROL_WIDTH = 260;

export type GeneralPaneProps = Readonly<{
  /** `undefined` means "still loading" — never render a guessed control. */
  settings: Readonly<Record<string, string>> | undefined;
  loadError: string | null;
  onSave: (patch: SettingsPatch) => void | Promise<void>;
  onRetryLoad: () => void;
  savedNoticeMs?: number;
}>;

type GeneralRowStatus =
  | Readonly<{ phase: 'idle' }>
  | Readonly<{ phase: 'saving'; value: number }>
  | Readonly<{ phase: 'saved'; at: number; value: number }>
  | Readonly<{ phase: 'failed'; message: string; value: number }>;

const GENERAL_IDLE: GeneralRowStatus = Object.freeze({ phase: 'idle' });

export function GeneralPane({
  settings, loadError, onSave, onRetryLoad, savedNoticeMs = SAVED_NOTICE_MS,
}: GeneralPaneProps) {
  const loaded = settings !== undefined;
  const incoming = taskBudgetDefaultFrom(settings ?? {});
  const [seed, setSeed] = useState<number | null>(null);
  const [draft, setDraft] = useState(incoming);
  const draftRef = useRef(draft);
  draftRef.current = draft;
  const sent = useRef<number | null>(null);
  const sequence = useRef(0);
  const [status, setStatus] = useState<GeneralRowStatus>(GENERAL_IDLE);
  const [changedElsewhere, setChangedElsewhere] = useState<number | null>(null);
  const statusRef = useRef(status);
  statusRef.current = status;

  if (loaded && seed !== incoming) {
    const previous = seed;
    setSeed(incoming);
    if (statusRef.current.phase !== 'saving') sent.current = null;
    if (statusRef.current.phase === 'saved' && statusRef.current.value !== incoming) setStatus(GENERAL_IDLE);
    if (previous !== null && draft !== previous && draft !== incoming
      && statusRef.current.phase !== 'saving'
      && (statusRef.current.phase === 'idle' || statusRef.current.value !== incoming)) {
      setChangedElsewhere(incoming);
    } else if (draft === previous || draft === incoming) setChangedElsewhere(null);
    setDraft((current) => (previous === null || current === previous ? incoming : current));
  }

  const base = seed ?? incoming;
  const commit = (value: number) => {
    if (!Number.isSafeInteger(value) || value < 1) return;
    if (value === (sent.current ?? base)) return;
    sent.current = value;
    setChangedElsewhere(null);
    const ticket = (sequence.current += 1);
    const settle = (next: GeneralRowStatus) => {
      if (sequence.current !== ticket) return;
      if (next.phase === 'failed') sent.current = null;
      setStatus(next);
    };
    setStatus({ phase: 'saving', value });
    void Promise.resolve(onSave({ [TASK_BUDGET_DEFAULT_KEY]: String(value) }))
      .then(() => settle({ phase: 'saved', at: Date.now(), value }))
      .catch((error: unknown) => settle({
        phase: 'failed',
        message: error instanceof Error ? error.message : 'Save failed.',
        value,
      }));
  };

  const pending = useRef({ draft, base, onSave });
  pending.current = { draft, base, onSave };
  useEffect(() => () => {
    const { draft: last, base: seeded, onSave: save } = pending.current;
    if (last === (sent.current ?? seeded)) return;
    const verdict = statusRef.current;
    if (verdict.phase === 'failed' && verdict.value === last) return;
    void Promise.resolve(save({ [TASK_BUDGET_DEFAULT_KEY]: String(last) })).catch(() => {
      // The next visit re-reads the authoritative value.
    });
  }, []);

  const savedAt = status.phase === 'saved' ? status.at : null;
  useEffect(() => {
    if (savedAt === null) return;
    const id = setTimeout(() => setStatus((current) => (
      current.phase === 'saved' && current.at === savedAt ? GENERAL_IDLE : current
    )), savedNoticeMs);
    return () => clearTimeout(id);
  }, [savedAt, savedNoticeMs]);

  const inputStatus = changedElsewhere !== null && draft !== changedElsewhere
    ? { type: 'warning' as const, message: `Changed elsewhere to ${changedElsewhere}. Your edit is not saved.` }
    : status.phase !== 'idle' && status.value === draft
    ? status.phase === 'failed'
      ? { type: 'error' as const, message: status.message }
      : status.phase === 'saved'
        ? { type: 'success' as const }
        : undefined
    : undefined;

  return (
    <SettingsPane
      category="general"
      title="General"
      lede="Workspace-wide defaults for task scheduling. Changes apply to work that has not started yet."
    >
      {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
      {!loaded && loadError === null && <AstryxText as="p" color="secondary">Loading settings…</AstryxText>}
      {loaded && (
        <SettingsList>
          <SettingRow
            title="Task concurrency"
            description="Per-track default; server and track-specific limits still apply."
            control={(
              <>
                <AstryxVisuallyHidden role="status">
                  {inputStatus?.type === 'success' ? 'Saved.' : ''}
                </AstryxVisuallyHidden>
                <AstryxNumberInput
                  label="Task concurrency"
                  isLabelHidden
                  value={draft}
                  min={1}
                  max={Number.MAX_SAFE_INTEGER}
                  step={1}
                  isIntegerOnly
                  units="tasks"
                  status={inputStatus}
                  onChange={(value) => {
                    draftRef.current = value;
                    setDraft(value);
                    setStatus((current) => (
                      current.phase === 'idle' || current.phase === 'saving' || current.value === value
                        ? current
                        : GENERAL_IDLE
                    ));
                  }}
                  onBlur={() => commit(draftRef.current)}
                  onEnter={() => commit(draftRef.current)}
                  width={CONTROL_WIDTH}
                  data-nc-state={status.phase === 'saving' ? 'busy' : undefined}
                />
              </>
            )}
          />
        </SettingsList>
      )}
    </SettingsPane>
  );
}

export type NetworkPaneProps = Readonly<{
  onOpenMobile: () => void;
  /** `undefined` means "still loading" — never render an empty form for it. */
  settings: Readonly<Record<string, string>> | undefined;
  loadError: string | null;
  /** Commits one key. The returned promise is the row's status: this pane follows it per field. */
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

/** What one row's last commit is doing, per field and derived from that field's own promise. A response whose sequence is no longer current is dropped rather than overwriting the newer outcome. */
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
  settings, loadError, onSave, onRetryLoad, onOpenMobile, savedNoticeMs = SAVED_NOTICE_MS,
}: NetworkPaneProps) {
  const loaded = settings !== undefined;
  const incoming: Draft = {
    http: settings?.[HTTP_PROXY_KEY] ?? '',
    https: settings?.[HTTPS_PROXY_KEY] ?? '',
  };

  // Seeding compares by value, not object identity: a query cache hands back a fresh object on every render.
  const [seed, setSeed] = useState<Draft | null>(null);
  const [draft, setDraft] = useState<Draft>({ http: '', https: '' });
  /** What this pane last told the server for each field, or `null` when nothing since the bag it holds: between a commit and its echo `base` alone would call the same value "changed". */
  const sent = useRef<Record<ProxyField, string | null>>({ http: null, https: null });

  const [status, setStatus] = useState<Readonly<Record<ProxyField, RowStatus>>>(
    { http: IDLE, https: IDLE },
  );
  const [changedElsewhere, setChangedElsewhere] = useState<Readonly<Record<ProxyField, boolean>>>({ http: false, https: false });
  /* Read by the re-seed block (render phase) and by the unmount cleanup, both
     of which need the *current* verdicts rather than a captured render's. */
  const statusRef = useRef(status);
  statusRef.current = status;

  if (loaded && (seed === null || seed.http !== incoming.http || seed.https !== incoming.https)) {
    const previous = seed;
    setSeed(incoming);
    /* A new bag makes the server's word the reference again — except for a field whose write is still out. */
    for (const field of PROXY_FIELDS) {
      if (statusRef.current[field].phase !== 'saving') sent.current[field] = null;
    }
    setChangedElsewhere((current) => {
      const next = { ...current };
      for (const field of PROXY_FIELDS) {
        const row = statusRef.current[field];
        if (previous !== null && draft[field] !== previous[field] && draft[field] !== incoming[field]
          && row.phase !== 'saving'
          && (row.phase === 'idle' || row.value !== incoming[field])) next[field] = true;
        else if (previous === null || draft[field] === previous[field] || draft[field] === incoming[field]) next[field] = false;
      }
      return next;
    });
    setStatus((current) => ({
      http: current.http.phase === 'saved' && current.http.value !== incoming.http ? IDLE : current.http,
      https: current.https.phase === 'saved' && current.https.value !== incoming.https ? IDLE : current.https,
    }));
    setDraft((current) => ({
      http: previous === null || current.http === previous.http ? incoming.http : current.http,
      https: previous === null || current.https === previous.https ? incoming.https : current.https,
    }));
  }

  const base = seed ?? { http: '', https: '' };
  const sequence = useRef<Record<ProxyField, number>>({ http: 0, https: 0 });
  const referenceFor = (field: ProxyField) => sent.current[field] ?? base[field];

  /** Commit on blur and Enter, never per keystroke; a value equal to the reference commits nothing. */
  const commit = (field: ProxyField, value: string) => {
    if (value === referenceFor(field)) return;
    sent.current[field] = value;
    setChangedElsewhere((current) => ({ ...current, [field]: false }));
    const ticket = (sequence.current[field] += 1);
    const settle = (next: RowStatus) => {
      // A response for a superseded commit says nothing about the current one.
      if (sequence.current[field] !== ticket) return;
      /* Rolled back inside the ticket check, not in the `catch`: an older request failing must not wipe the reference of a newer in-flight value. */
      if (next.phase === 'failed') sent.current[field] = null;
      setStatus((current) => ({ ...current, [field]: next }));
    };
    setStatus((current) => ({ ...current, [field]: { phase: 'saving', value } }));
    void Promise.resolve(onSave({ [PROXY_KEY_OF[field]]: value === '' ? null : value }))
      .then(() => { settle({ phase: 'saved', at: Date.now(), value }); })
      .catch((error: unknown) => {
        /* Clearing `sent` on failure is what lets the reader retry with refocus + Enter. */
        settle({ phase: 'failed', message: error instanceof Error ? error.message : 'Save failed.', value });
      });
  };

  /* Closing the dialog commits what is in the fields: unmounting a focused input fires no blur. The refs make the cleanup read the last render's values. */
  const pending = useRef({ draft, base, onSave });
  pending.current = { draft, base, onSave };
  useEffect(() => () => {
    const { draft: last, base: seeded, onSave: save } = pending.current;
    for (const field of PROXY_FIELDS) {
      // The same guard the blur path uses: what was already sent is not resent
      // just because the bag has not echoed it back yet.
      if (last[field] === (sent.current[field] ?? seeded[field])) continue;
      /* Never the value the reader just watched fail: closing the dialog must not become a silent retry. */
      const verdict = statusRef.current[field];
      if (verdict.phase === 'failed' && verdict.value === last[field]) continue;
      const value = last[field];
      void Promise.resolve(save({ [PROXY_KEY_OF[field]]: value === '' ? null : value })).catch(() => {
        // Nothing is mounted to report to; the next visit re-reads the bag.
      });
    }
  }, []);

  /* Per row, keyed on that row's saved timestamp: one shared timer restarted whenever the other row changed. */
  useRetiringNotice('http', status.http, setStatus, savedNoticeMs);
  useRetiringNotice('https', status.https, setStatus, savedNoticeMs);

  /** The confirmation is the tick and nothing else; the word goes to the always-mounted live region beside the field. */
  const statusFor = (field: ProxyField) => {
    if (changedElsewhere[field] && draft[field] !== base[field]) {
      return { type: 'warning' as const, message: 'Changed elsewhere. Your edit is not saved.' };
    }
    const row = status[field];
    if (row.phase === 'idle') return undefined;
    /* A verdict describes the value it was for: once the draft moved on, neither the tick nor the error is about what is on screen. */
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
            onChange={(value) => {
              setDraft({ ...draft, [field]: value });
              /* Withdrawn, not hidden: typing away from a failed value and back must not bring the old verdict back. An in-flight commit keeps its status. */
              setStatus((current) => {
                const row = current[field];
                if (row.phase === 'idle' || row.phase === 'saving') return current;
                return row.value === value ? current : { ...current, [field]: IDLE };
              });
            }}
            onBlur={() => commit(field, draft[field])}
            onKeyDown={(event) => { if (event.key === 'Enter') commit(field, draft[field]); }}
            width={CONTROL_WIDTH}
            /* The field stays editable while its write is in flight: blocking it would drop the next keystroke. */
            data-nc-state={status[field].phase === 'saving' ? 'busy' : undefined}
          />
        </>
      )}
    />
  );

  return (
    <SettingsPane
      category="network"
      title="Network"
      lede="Connect your phone and configure proxies used when launching new agent cards. Proxy changes save when you leave the field."
    >
      {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
      {/* A loading line, never an empty field: an empty form would let the reader save blanks over real values. */}
      {!loaded && loadError === null && <AstryxText as="p" color="secondary">Loading settings…</AstryxText>}
      {loaded && <SettingsList>
        {PROXY_FIELDS.map((field) => proxyRow(field))}
        <SettingRow title="Mobile connection" description="Pair your phone by scanning a QR code." onOpen={onOpenMobile} />
      </SettingsList>}
    </SettingsPane>
  );
}

export function AppearancePane({ themeMode, onThemeModeChange }: Readonly<{
  themeMode: ThemeMode;
  onThemeModeChange: (mode: ThemeMode) => void;
}>) {
  return (
    <SettingsPane category="appearance" title="Appearance" lede="How this device paints the app. Not shared with your other devices.">
      <SettingsList>
        <SettingRow
          title="Theme"
          description="System follows your operating system's setting."
          control={(
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

export function AboutPane() {
  return (
    <SettingsPane category="about" title="About" lede="What this build is. Read-only.">
      <SettingsList>
        <SettingRow title="Version" control={<span className={styles.aboutValue}>{__NC_VERSION__}</span>} />
        <SettingRow title="Build" control={<span className={styles.aboutValue}>{__NC_BUILD__}</span>} />
      </SettingsList>
    </SettingsPane>
  );
}
