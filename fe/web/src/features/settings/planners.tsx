// Settings › Planners — whether each Planner provider can run on this server right now, and the fix
// when one cannot (#1817), plus the next turn's execution budget (#2618).

import { Badge as AstryxBadge } from '@astryxdesign/core/Badge';
import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { Text as AstryxText } from '@astryxdesign/core/Text';

import type { AgentProvider } from '../../../../core/api/schemas.ts';
import {
  unavailableCreationNote, type ProviderAvailability, type ManagedProviderAvailability, type AuthenticationRetryResponse,
} from '../../../../core/domain/agent-providers.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { plannerTurnDuration } from '../../../../core/domain/settings.ts';
import { SettingRow, SettingsList, SettingsPane } from './public.tsx';
import { SettingEditor, type SettingEditorCopy, type SettingEditorProps } from './setting-editor.tsx';
import styles from './settings.module.css';

export type PlannersPaneProps = Readonly<{
  turnDuration: SettingEditorProps;
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

const TURN_DURATION: SettingEditorCopy = Object.freeze({
  label: 'Maximum turn duration (minutes)',
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
  providers, loadError, onRetryLoad, onRecheck, rechecking, recheckError, authenticationRecovery, turnDuration,
}: PlannersPaneProps) {
  const checkedAt = providers === undefined ? null : oldestCheck(providers.map(({ availability }) => availability));
  return (
    <SettingsPane
      category="planners"
      title="Planners"
      lede="Which Planner providers this server can run right now, and how to fix one that cannot."
    >
      <SettingsList>
        <SettingRow title="Maximum turn duration" titleSuffix={TURN_DURATION.unit}
          control={<SettingEditor {...turnDuration} copy={TURN_DURATION} />} />
      </SettingsList>
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
