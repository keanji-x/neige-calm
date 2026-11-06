// Settings › Planners — whether each Planner provider can run on this server right now, and the fix
// when one cannot (#1817), plus the next turn's execution budget (#2618).

import { Badge as AstryxBadge } from '@astryxdesign/core/Badge';
import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { Text as AstryxText } from '@astryxdesign/core/Text';
import { TextInput as AstryxTextInput } from '@astryxdesign/core/TextInput';
import { useRef } from 'react';

import type { AgentProvider } from '../../../../core/api/schemas.ts';
import {
  unavailableCreationNote, type ProviderAvailability, type ManagedProviderAvailability, type AuthenticationRetryResponse,
} from '../../../../core/domain/agent-providers.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { mailPerHour, plannerTurnDuration, SETTINGS_FAILURES, SETTINGS_TEXT } from '../../../../core/domain/settings.ts';
import { writeFailureText } from '../../../../core/domain/failure-class.ts';
import { CONTROL_WIDTH, SettingRow, SettingsList, SettingsPane } from './public.tsx';
import styles from './settings.module.css';

export type PlannersPaneProps = Readonly<{
  turnDuration: PlannerSettingProps;
  mailQuota: PlannerSettingProps;
  /** `undefined` means "still checking" — never render a guessed status. */
  providers: readonly PlannerProvider[] | undefined;
  loadError: string | null;
  onRetryLoad: () => void;
  /** Run every check again now (`refresh=true`). */
  onRecheck: () => void;
  rechecking: boolean;
  /** Why the last Recheck failed; the previous answer stays on screen. */
  recheckError: string | null;
  /** An app-injected capability; the pane never infers a provider's recovery policy. */
  authenticationRecovery: Readonly<{
    provider: AgentProvider;
    pending: boolean;
    error: string | null;
    notices: AuthenticationRetryResponse['recovery_notices'];
    onRetry: (revision: string) => void;
    onOpenConversation: (notice: AuthenticationRetryResponse['recovery_notices'][number]) => void;
  }> | null;
}>;

/** Availability and the one required capability consumed by Settings. */
type PlannerProvider = Readonly<{
  availability: ProviderAvailability;
  accepts_unavailable_blank_track: ManagedProviderAvailability['accepts_unavailable_blank_track'];
}>;

/** Only this setting's value and capabilities; app owns the bag, transport and invalidation. */
type PlannerSettingProps = Readonly<{
  value: string | undefined;
  /** Successful-read counter: even an unchanged server value can supersede a save confirmation. */
  readRevision: number;
  loaded: boolean;
  loadError: string | null;
  onRetryLoad: () => void;
  onSave: (value: string | null) => Promise<void>;
}>;

type PlannerSettingCopy = Readonly<{
  title: string;
  unit: string;
  inputMode: 'numeric' | 'text';
  name: string;
  description: string;
  resetLabel: string;
  parse: typeof mailPerHour;
}>;

const TURN_DURATION: PlannerSettingCopy = Object.freeze({
  title: 'Maximum turn duration',
  unit: 'minutes',
  inputMode: 'numeric',
  name: 'duration',
  description: 'Default: 30 minutes. Choose 1–240 minutes. Changes apply to the next turn; retrying an unconfirmed start keeps its frozen budget. Hold waiting time does not count. Backends without steer support receive only the budget reminder at the start of the turn, without a mid-turn warning.',
  resetLabel: 'Restore default duration',
  parse(value: string | undefined) {
    const parsed = plannerTurnDuration(value);
    return parsed.ok ? { ok: true as const, text: String(parsed.minutes) } : parsed;
  },
});

const MAIL_QUOTA: PlannerSettingCopy = Object.freeze({
  title: 'Track mail limit',
  unit: 'messages per hour',
  inputMode: 'text',
  name: 'mail limit',
  description: 'Use a rate like 5/h (the default). Each Track shares its quota across sends and replies in the last 60 minutes.',
  resetLabel: 'Restore default mail limit',
  parse(value: string | undefined) { return mailPerHour(value); },
});

function settingText(parse: PlannerSettingCopy['parse'], value: string | undefined): string {
  const parsed = parse(value);
  return parsed.ok ? parsed.text : value ?? '';
}

type SettingSave =
  | Readonly<{ phase: 'idle' }>
  | Readonly<{ phase: 'pending' | 'saved'; value: string }>
  | Readonly<{ phase: 'failed'; value: string; message: string }>;

function PlannerSetting({ value, readRevision, loaded, loadError, onRetryLoad, onSave, copy }:
  PlannerSettingProps & { copy: PlannerSettingCopy }) {
  const { title, unit, inputMode, name, description, resetLabel, parse } = copy;
  const [seed, setSeed] = useState({ loaded, value, readRevision });
  const [draft, setDraft] = useState(loaded ? settingText(parse, value) : '');
  const [changedElsewhere, setChangedElsewhere] = useState(false);
  const [validationError, setValidationError] = useState<string | null>(null);
  const [save, setSave] = useState<SettingSave>({ phase: 'idle' });
  // One active write. Editing stays available, but no queued or automatically replayed PUT exists.
  const busy = useRef(false);
  const acknowledged = useRef<string | undefined>(undefined);
  const latestRead = useRef({ value, readRevision });
  latestRead.current = { value, readRevision };
  if (loaded && (!seed.loaded || seed.value !== value || seed.readRevision !== readRevision)) {
    const incoming = settingText(parse, value);
    const clean = !seed.loaded || draft === settingText(parse, seed.value);
    setSeed({ loaded, value, readRevision });
    acknowledged.current = undefined;
    if (clean && !busy.current && save.phase === 'idle') {
      setDraft(incoming);
      setChangedElsewhere(false);
      setSave({ phase: 'idle' });
      setValidationError(null);
    } else {
      setChangedElsewhere(draft !== incoming && !(save.phase === 'pending' && save.value === incoming));
      if (save.phase === 'saved' && save.value !== incoming) setSave({ phase: 'idle' });
    }
  }
  const commit = (reset: boolean, explicit: boolean) => {
    if (busy.current || !loaded) return;
    const parsed = parse(reset ? undefined : draft);
    if (!parsed.ok) { setValidationError(parsed.error); return; }
    const text = parsed.text;
    // Leaving a failed field is not permission to silently retry it.
    if (!reset && !explicit && save.phase === 'failed' && save.value === text) return;
    if (!reset && save.phase !== 'failed' && (text === acknowledged.current
      || (parse(value).ok && text === settingText(parse, value)))) return;
    busy.current = true;
    setDraft(text);
    setChangedElsewhere(false);
    setValidationError(null);
    setSave({ phase: 'pending', value: text });
    const submittedRevision = readRevision;
    void (async () => {
      try {
        await onSave(reset ? null : text);
        const read = latestRead.current;
        if (read.readRevision !== submittedRevision && settingText(parse, read.value) !== text) {
          acknowledged.current = undefined;
          setSave({ phase: 'idle' });
          return;
        }
        acknowledged.current = text;
        setSave({ phase: 'saved', value: text });
      } catch (error: unknown) {
        acknowledged.current = undefined;
        setSave({ phase: 'failed', value: text,
          message: writeFailureText(SETTINGS_FAILURES, SETTINGS_TEXT)(error) ?? SETTINGS_TEXT.unknown });
      } finally { busy.current = false; }
    })();
  };
  const stored = parse(value);
  const parsedDraft = parse(draft);
  const inputError = parsedDraft.ok ? null
    : !stored.ok && draft === settingText(parse, value)
      ? `Stored ${name} is invalid. ${stored.error} Restore default to repair it.`
      : validationError ?? parsedDraft.error;
  return <SettingsList>
    <SettingRow title={title} titleSuffix={unit}
      control={<div className={styles.durationControl}>
        {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
        {!loaded ? loadError === null && <span role="status">Loading {name}…</span> : <>
          <AstryxTextInput label={`${title} (${unit})`} value={draft} width={CONTROL_WIDTH}
            ref={(input) => { if (input !== null) input.inputMode = inputMode; }}
            description={description}
            status={inputError === null ? undefined : { type: 'error', message: inputError }}
            onChange={(next) => { setDraft(next); setValidationError(null); }}
            onBlur={() => commit(false, false)}
            onEnter={() => commit(false, true)} />
          <AstryxButton label={resetLabel} variant="secondary" isDisabled={save.phase === 'pending'}
            onPointerDown={(event) => event.preventDefault()} onClick={() => commit(true, true)}>Restore default</AstryxButton>
          {save.phase === 'failed' && <p className={styles.error} role="alert">{save.value}{inputMode === 'numeric' ? ` ${unit}` : ''}: {save.message} Press Enter to try again or restore default.</p>}
          <span role="status">{save.phase === 'pending' ? `Saving ${save.value}${inputMode === 'numeric' ? ` ${unit}` : ''}…`
            : save.phase === 'saved' && save.value === draft ? 'Saved.'
              : draft !== settingText(parse, value) ? 'Your edit is not saved.' : ''}</span>
          {save.phase === 'pending' && save.value !== draft && <span role="status">Your new edit is not saved. Press Enter after this save finishes.</span>}
          {changedElsewhere && <span role="status">Changed elsewhere. Your edit is retained; review it before saving.</span>}
        </>}
      </div>} />
  </SettingsList>;
}

const PROVIDER_LABELS: Readonly<Record<AgentProvider, string>> = Object.freeze({ codex: 'Codex', claude: 'Claude', opencode: 'OpenCode', grok: 'Grok', antigravity: 'Antigravity' });

/** `unavailable` is warning, not error: a login or a config fix away. `not_configured` is a server fact, not a fault. */
const STATUS_BADGES: Readonly<Record<ProviderAvailability['status'], Readonly<{
  label: string; variant: 'success' | 'warning' | 'neutral';
}>>> = Object.freeze({
  ready: Object.freeze({ label: 'Ready', variant: 'success' }),
  unavailable: Object.freeze({ label: 'Unavailable', variant: 'warning' }),
  not_configured: Object.freeze({ label: 'Not configured', variant: 'neutral' }),
});

const READY_DESCRIPTION = 'Passed every check; new tracks can use it.';

/** The server's reason and its blank-creation advice. */
function describe({ availability: entry, accepts_unavailable_blank_track }: PlannerProvider): string {
  if (entry.authentication_notice !== null) return entry.authentication_notice.text;
  if (entry.reason === null) return READY_DESCRIPTION;
  return entry.status === 'unavailable' && accepts_unavailable_blank_track
    ? `${entry.reason} ${unavailableCreationNote(PROVIDER_LABELS[entry.provider])}`
    : entry.reason;
}

/** The oldest answer on screen: what "last checked" can honestly claim for the whole list. */
function oldestCheck(providers: readonly ProviderAvailability[]): number | null {
  return providers.reduce<number | null>(
    (oldest, entry) => oldest === null || entry.checked_at_ms < oldest ? entry.checked_at_ms : oldest, null);
}

export function PlannersPane({
  providers, loadError, onRetryLoad, onRecheck, rechecking, recheckError, authenticationRecovery, turnDuration, mailQuota,
}: PlannersPaneProps) {
  const checkedAt = providers === undefined ? null : oldestCheck(providers.map(({ availability }) => availability));
  return (
    <SettingsPane
      category="planners"
      title="Planners"
      lede="Which Planner providers this server can run right now, and how to fix one that cannot."
    >
      <PlannerSetting {...turnDuration} copy={TURN_DURATION} />
      <PlannerSetting {...mailQuota} copy={MAIL_QUOTA} />
      {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
      {providers === undefined
        ? loadError === null && <AstryxText as="p" color="secondary">Checking providers…</AstryxText>
        : (
          <SettingsList>
            {providers.map(({ availability: entry, accepts_unavailable_blank_track }) => (
              <SettingRow
                key={entry.provider}
                title={PROVIDER_LABELS[entry.provider]}
                /* A node, not a string: a string description is cut to one line, and a reason is a fix to read whole. */
                description={<span className={styles.plannerReason}>{describe({ availability: entry, accepts_unavailable_blank_track })}</span>}
                control={<>
                  <AstryxBadge className={styles.pluginStateChip}
                    variant={STATUS_BADGES[entry.status].variant} label={STATUS_BADGES[entry.status].label} />
                  {authenticationRecovery?.provider === entry.provider && entry.authentication_notice?.kind === 'sign_in_required'
                    && <AstryxButton label="Retry queued messages after server sign-in" variant="secondary"
                      isLoading={authenticationRecovery.pending} onClick={() => authenticationRecovery.onRetry(entry.authentication_notice!.revision)}>
                      Retry after server sign-in
                    </AstryxButton>}
                </>}
              />
            ))}
            <SettingRow
              title="Recheck"
              description={checkedAt === null
                ? 'Runs every check again now.'
                : `Last checked at ${new Date(checkedAt).toLocaleTimeString()}. The server keeps an answer for 30 seconds.`}
              control={<AstryxButton label="Recheck planners" variant="secondary" isLoading={rechecking}
                onClick={onRecheck}>Recheck</AstryxButton>}
            />
          </SettingsList>
        )}
      {authenticationRecovery?.error !== null && authenticationRecovery?.error !== undefined
        && <p className={styles.error} role="alert">{authenticationRecovery.error}</p>}
      {authenticationRecovery?.notices.map((notice) => <ErrorBox key={notice.card_id}
        message={`${notice.title}: ${notice.text}`} actionLabel="Open conversation"
        onRetry={() => authenticationRecovery.onOpenConversation(notice)} />)}
      {recheckError !== null && <p className={styles.error} role="alert">{recheckError}</p>}
    </SettingsPane>
  );
}
