// Settings › Planners — whether each Planner provider can run on this server right now, and the fix
// when one cannot (#1817). Read-only apart from Recheck; the reasons are the server's own sentences.

import { Badge as AstryxBadge } from '@astryxdesign/core/Badge';
import { Button as AstryxButton } from '@astryxdesign/core/Button';
import { Text as AstryxText } from '@astryxdesign/core/Text';

import type { AgentProvider } from '../../../../core/api/schemas.ts';
import type { ProviderAvailability } from '../../../../core/domain/agent-providers.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { SettingRow, SettingsList, SettingsPane } from './public.tsx';
import styles from './settings.module.css';

export type PlannersPaneProps = Readonly<{
  /** `undefined` means "still checking" — never render a guessed status. */
  providers: readonly ProviderAvailability[] | undefined;
  loadError: string | null;
  onRetryLoad: () => void;
  /** Run every check again now (`refresh=true`). */
  onRecheck: () => void;
  rechecking: boolean;
  /** Why the last Recheck failed; the previous answer stays on screen. */
  recheckError: string | null;
}>;

const PROVIDER_LABELS: Readonly<Record<AgentProvider, string>> = Object.freeze({ codex: 'Codex', claude: 'Claude' });

/** `unavailable` is warning, not error: a login or a config fix away. `not_configured` is a server fact, not a fault. */
const STATUS_BADGES: Readonly<Record<ProviderAvailability['status'], Readonly<{
  label: string; variant: 'success' | 'warning' | 'neutral';
}>>> = Object.freeze({
  ready: Object.freeze({ label: 'Ready', variant: 'success' }),
  unavailable: Object.freeze({ label: 'Unavailable', variant: 'warning' }),
  not_configured: Object.freeze({ label: 'Not configured', variant: 'neutral' }),
});

const READY_DESCRIPTION = 'Passed every check; new tracks can use it.';

/** The oldest answer on screen: what "last checked" can honestly claim for the whole list. */
function oldestCheck(providers: readonly ProviderAvailability[]): number | null {
  return providers.reduce<number | null>(
    (oldest, entry) => oldest === null || entry.checked_at_ms < oldest ? entry.checked_at_ms : oldest, null);
}

export function PlannersPane({
  providers, loadError, onRetryLoad, onRecheck, rechecking, recheckError,
}: PlannersPaneProps) {
  const checkedAt = providers === undefined ? null : oldestCheck(providers);
  return (
    <SettingsPane
      category="planners"
      title="Planners"
      lede="Which Planner providers this server can run right now, and how to fix one that cannot."
    >
      {loadError !== null && <ErrorBox message={loadError} onRetry={onRetryLoad} />}
      {providers === undefined
        ? loadError === null && <AstryxText as="p" color="secondary">Checking providers…</AstryxText>
        : (
          <SettingsList>
            {providers.map((entry) => (
              <SettingRow
                key={entry.provider}
                title={PROVIDER_LABELS[entry.provider]}
                description={entry.reason ?? READY_DESCRIPTION}
                control={<AstryxBadge className={styles.pluginStateChip}
                  variant={STATUS_BADGES[entry.status].variant} label={STATUS_BADGES[entry.status].label} />}
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
      {recheckError !== null && <p className={styles.error} role="alert">{recheckError}</p>}
    </SettingsPane>
  );
}
