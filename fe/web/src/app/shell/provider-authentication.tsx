// Shell-owned notification from the provider's declared diagnostic contract.
import { useQuery } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { agentProvidersQueryOptions } from '../providers/agent-providers.ts';
import styles from './provider-authentication.module.css';

export function ProviderAuthenticationNotice({ transport, unauthorized, onOpenPlanners }: Readonly<{
  transport: ApiTransportPort;
  unauthorized: UnauthorizedChannel;
  onOpenPlanners: () => void;
}>) {
  const query = useQuery({
    ...agentProvidersQueryOptions(transport, unauthorized),
    refetchInterval: 15_000,
    refetchIntervalInBackground: false,
    retry: false,
  });
  const notices = query.data?.flatMap((entry) => entry.authentication_notice === null
    ? [] : [{ provider: entry.provider, notice: entry.authentication_notice }]) ?? [];
  if (notices.length === 0) return null;
  return <div className={styles.notices} aria-label="Provider sign-in notifications">
    {notices.map(({ provider, notice }) => <div key={provider} data-nc-auth-notice={notice.kind}>
      <ErrorBox message={notice.text} onRetry={onOpenPlanners} actionLabel="Settings"
        description={notice.kind === 'sign_in_required'
          ? 'Queued messages are kept. After signing in for this server, open Settings to allow them to retry.'
          : notice.kind === 'retry_requested'
            ? 'If the current sign-in still fails, the messages will stay queued.'
          : notice.kind === 'refresh_error_reported'
            ? 'Messages can still run. Check the server’s sign-in if they stop.'
            : 'Queued messages are kept while the server’s saved status is unavailable.'} />
    </div>)}
  </div>;
}
