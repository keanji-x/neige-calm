import { useQueries, useQuery, useQueryClient } from '@tanstack/react-query';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { managedProviderProblem, managedProvidersOperation } from '../../../../core/domain/agent-providers.ts';
import { WEB_COMPAT_VERSION } from './public.tsx';
import { serverVersionOperation, modelCatalogQueryOptions, runOperation } from './queries.ts';

export const managedProvidersQueryKey = Object.freeze(['managed-providers'] as const);

export function managedProvidersQueryOptions(transport: ApiTransportPort, unauthorized: UnauthorizedChannel) {
  return { queryKey: managedProvidersQueryKey,
    queryFn: ({ signal }: { signal: AbortSignal }) => runOperation(transport, { ...managedProvidersOperation(false), signal }, unauthorized) };
}

/** Backend registrations own the menu membership; catalogs use their reported provider identity. */
export function useManagedProviders(transport: ApiTransportPort, unauthorized: UnauthorizedChannel, enabled: boolean) {
  const client = useQueryClient();
  const query = useQuery({ ...managedProvidersQueryOptions(transport, unauthorized), enabled });
  const version = useQuery({ queryKey: ['server-version'], queryFn: () => runOperation(transport, serverVersionOperation(), unauthorized), enabled, retry: false });
  const supportsAssistantProviders = !version.isError && version.data?.assistantProviders === true
    && version.data.webCompatVersion >= WEB_COMPAT_VERSION && version.data.minWebCompatVersion <= WEB_COMPAT_VERSION;
  const answers = query.isError ? undefined : query.data;
  const catalogs = useQueries({ queries: (answers ?? []).map(({ availability, capabilities }) => ({
    ...modelCatalogQueryOptions(transport, { kind: 'provider', provider: availability.provider }, unauthorized),
    enabled: enabled && capabilities.managed_conversation && managedProviderProblem(answers, availability.provider) === null,
  })) });
  const recheck = async () => {
    const next = await runOperation(transport, managedProvidersOperation(true), unauthorized);
    client.setQueryData(managedProvidersQueryKey, next);
    return next;
  };
  return { recheck, answers, version, supportsAssistantProviders, loading: query.isPending && !query.isError, groups: (answers ?? []).map(({ availability }, index) => ({
    provider: availability.provider, availability, catalog: catalogs[index].data ?? null,
    selectionProblem: managedProviderProblem(answers, availability.provider),
  })) };
}
