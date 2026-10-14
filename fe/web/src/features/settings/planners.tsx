// Settings › Planners — whether each Planner provider can run on this server right now, and the fix
// when one cannot (#1817), plus the next turn's execution budget (#2618).

import { Badge as AstryxBadge } from '@astryxdesign/core/Badge';
import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { Text as AstryxText } from '@astryxdesign/core/Text';
import { TextInput as AstryxTextInput } from '@astryxdesign/core/TextInput';
import { useRef } from 'react';

import type { AgentProvider } from '../../../../core/api/schemas.ts';
import {
  CREATE_REFUSED_WHEN_UNAVAILABLE, STILL_CREATES_NOTE, type ProviderAvailability, type AuthenticationRetryResponse,
} from '../../../../core/domain/agent-providers.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { useState } from '../../ui/state/public.ts';
import { plannerTurnDuration, SETTINGS_FAILURES, SETTINGS_TEXT } from '../../../../core/domain/settings.ts';
import { writeFailureText } from '../../../../core/domain/failure-class.ts';
import { CONTROL_WIDTH, SettingRow, SettingsList, SettingsPane } from './public.tsx';
import styles from './settings.module.css';

export type PlannersPaneProps = Readonly<{
  turnDuration: PlannerTurnDurationProps;
  /** `undefined` means "still checking" — never render a guessed status. */
  providers: readonly ProviderAvailability[] | undefined;
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

/** Only this setting's value and capabilities; app owns the bag, transport and invalidation. */
type PlannerTurnDurationProps = Readonly<{
  value: string | undefined;
  /** Successful-read counter: even an unchanged server value can supersede a save confirmation. */
  readRevision: number;
  loaded: boolean;
  loadError: string | null;
  onRetryLoad: () => void;
  onSave: (value: string | null) => Promise<void>;
}>;

function durationText(value: string | undefined): string {
  const parsed = plannerTurnDuration(value);
  return parsed.ok ? String(parsed.minutes) : value!;
}

type DurationSave =
  | Readonly<{ phase: 'idle' }>
  | Readonly<{ phase: 'pending' | 'saved'; value: string }>
  | Readonly<{ phase: 'failed'; value: string; message: string }>;

function PlannerTurnDuration({ value, readRevision, loaded, loadError, onRetryLoad, onSave }: PlannerTurnDurationProps) {
  const [seed, setSeed] = useState({ loaded, value, readRevision });
  const [draft, setDraft] = useState(loaded ? durationText(value) : '');
  const [changedElsewhere, setChangedElsewhere] = useState(false);
  const [validationError, setValidationError] = useState<string | null>(null);
  const [save, setSave] = useState<DurationSave>({ phase: 'idle' });
  // One active write. Editing stays available, but no queued or automatically replayed PUT exists.
  const busy = useRef(false);
  const acknowledged = useRef<string | undefined>(undefined);
  const latestRead = useRef({ value, readRevision });
  latestRead.current = { value, readRevision };
  if (loaded && (!seed.loaded || seed.value !== value || seed.readRevision !== readRevision)) {
    const incoming = durationText(value);
    const clean = !seed.loaded || draft === durationText(seed.value);
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
    const parsed = plannerTurnDuration(reset ? undefined : draft);
    if (!parsed.ok) { setValidationError(parsed.error); return; }
    const text = String(parsed.minutes);
    // Leaving a failed field is not permission to silently retry it.
    if (!reset && !explicit && save.phase === 'failed' && save.value === text) return;
    if (!reset && save.phase !== 'failed' && (text === acknowledged.current
      || (plannerTurnDuration(value).ok && text === durationText(value)))) return;
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
        if (read.readRevision !== submittedRevision && durationText(read.value) !== text) {
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
  const stored = plannerTurnDuration(value);
  const parsedDraft = plannerTurnDuration(draft);
  const inputError = parsedDraft.ok ? null
    : !stored.ok && draft === durationText(value)
      ? `Stored duration is invalid. ${stored.error} Restore default to repair it.`
      : validationError ?? parsedDraft.error;
  return <SettingsList>
    <SettingRow title="Maximum turn duration" titleSuffix="minutes"
      control={<div className={styles.durationControl}>
        {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
        {!loaded ? loadError === null && <span role="status">Loading duration…</span> : <>
          <AstryxTextInput label="Maximum turn duration (minutes)" value={draft} width={CONTROL_WIDTH}
            ref={(input) => { if (input !== null) input.inputMode = 'numeric'; }}
            description="Default: 30 minutes. Choose 1–240 minutes. Changes apply to the next turn; retrying an unconfirmed start keeps its frozen budget. Hold waiting time does not count. Backends without steer support receive only the budget reminder at the start of the turn, without a mid-turn warning."
            status={inputError === null ? undefined : { type: 'error', message: inputError }}
            onChange={(next) => { setDraft(next); setValidationError(null); }}
            onBlur={() => commit(false, false)}
            onKeyDown={(event) => { if (event.key === 'Enter') { event.preventDefault(); commit(false, true); } }} />
          <AstryxButton label="Restore default duration" variant="secondary" isDisabled={save.phase === 'pending'}
            onPointerDown={(event) => event.preventDefault()} onClick={() => commit(true, true)}>Restore default</AstryxButton>
          {save.phase === 'failed' && <p className={styles.error} role="alert">{save.value} minutes: {save.message} Press Enter to try again or restore default.</p>}
          <span role="status">{save.phase === 'pending' ? `Saving ${save.value} minutes…`
            : save.phase === 'saved' && save.value === draft ? 'Saved.'
              : draft !== durationText(value) ? 'Your edit is not saved.' : ''}</span>
          {save.phase === 'pending' && save.value !== draft && <span role="status">Your new edit is not saved. Press Enter after this save finishes.</span>}
          {changedElsewhere && <span role="status">Changed elsewhere. Your edit is retained; review it before saving.</span>}
        </>}
      </div>} />
  </SettingsList>;
}

const PROVIDER_LABELS: Readonly<Record<AgentProvider, string>> = Object.freeze({ codex: 'Codex', claude: 'Claude', opencode: 'OpenCode' });

/** `unavailable` is warning, not error: a login or a config fix away. `not_configured` is a server fact, not a fault. */
const STATUS_BADGES: Readonly<Record<ProviderAvailability['status'], Readonly<{
  label: string; variant: 'success' | 'warning' | 'neutral';
}>>> = Object.freeze({
  ready: Object.freeze({ label: 'Ready', variant: 'success' }),
  unavailable: Object.freeze({ label: 'Unavailable', variant: 'warning' }),
  not_configured: Object.freeze({ label: 'Not configured', variant: 'neutral' }),
});

const READY_DESCRIPTION = 'Passed every check; new tracks can use it.';

/** The server's reason, and for a provider create still accepts (Codex), that it still does (#1817). */
function describe(entry: ProviderAvailability): string {
  if (entry.authentication_notice !== null) return entry.authentication_notice.text;
  if (entry.reason === null) return READY_DESCRIPTION;
  return entry.status === 'unavailable' && !CREATE_REFUSED_WHEN_UNAVAILABLE[entry.provider]
    ? `${entry.reason} ${STILL_CREATES_NOTE}`
    : entry.reason;
}

/** The oldest answer on screen: what "last checked" can honestly claim for the whole list. */
function oldestCheck(providers: readonly ProviderAvailability[]): number | null {
  return providers.reduce<number | null>(
    (oldest, entry) => oldest === null || entry.checked_at_ms < oldest ? entry.checked_at_ms : oldest, null);
}

export function PlannersPane({
  providers, loadError, onRetryLoad, onRecheck, rechecking, recheckError, authenticationRecovery, turnDuration,
}: PlannersPaneProps) {
  const checkedAt = providers === undefined ? null : oldestCheck(providers);
  return (
    <SettingsPane
      category="planners"
      title="Planners"
      lede="Which Planner providers this server can run right now, and how to fix one that cannot."
    >
      <PlannerTurnDuration {...turnDuration} />
      {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
      {providers === undefined
        ? loadError === null && <AstryxText as="p" color="secondary">Checking providers…</AstryxText>
        : (
          <SettingsList>
            {providers.map((entry) => (
              <SettingRow
                key={entry.provider}
                title={PROVIDER_LABELS[entry.provider]}
                /* A node, not a string: a string description is cut to one line, and a reason is a fix to read whole. */
                description={<span className={styles.plannerReason}>{describe(entry)}</span>}
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
