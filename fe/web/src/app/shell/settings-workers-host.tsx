// Settings owns drafts; app injects verified identity, shared queries and classified writes.
import { useQueries, useQuery, useQueryClient } from '@tanstack/react-query';
import { DEPLOYMENT_OWNER_USER_ID } from '../../../../core/api/auth.ts';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { readWriteFailure } from '../../../../core/domain/failure-class.ts';
import { readErrorText } from '../../../../core/domain/read-failure.ts';
import { SETTINGS_FAILURES, SETTINGS_TEXT } from '../../../../core/domain/settings.ts';
import { WORKER_MODEL_PROVIDERS, workerModelDefaultsKey, workerModelDefaultsPatch } from '../../../../core/domain/worker-model-defaults.ts';
import { WorkersPane } from '../../features/settings/workers.tsx';
import { useCurrentUser } from '../auth/current-user.tsx';
import { modelCatalogQueryOptions, queryKeys, settingsQueryOptions, useSettingsMutation } from '../providers/queries.ts';

/** Queries follow the visit; save never writes a PUT response into the shared Settings cache. */
export function WorkersPaneHost({ transport, unauthorized }: Readonly<{
  transport: ApiTransportPort; unauthorized: UnauthorizedChannel;
}>) {
  const owner = useCurrentUser()?.userId === DEPLOYMENT_OWNER_USER_ID;
  const client = useQueryClient();
  const settings = useQuery({ ...settingsQueryOptions(transport, unauthorized), enabled: owner, notifyOnChangeProps: 'all' });
  const catalogs = useQueries({ queries: WORKER_MODEL_PROVIDERS.map(provider => ({
    ...modelCatalogQueryOptions(transport, { kind: 'provider', provider }, unauthorized), enabled: owner,
  })) });
  const save = useSettingsMutation(transport, unauthorized);
  return <WorkersPane loaded={owner && settings.data !== undefined}
    readRevision={client.getQueryState(queryKeys.settings())?.dataUpdateCount ?? 0}
    loadError={settings.error === null ? null : readErrorText(settings.error, 'Settings are unavailable.')}
    onRetryLoad={() => { if (owner) void settings.refetch(); }}
    providers={WORKER_MODEL_PROVIDERS.map((provider, index) => ({ provider,
      value: settings.data?.settings[workerModelDefaultsKey(provider)], catalog: catalogs[index].data,
      catalogError: catalogs[index].error === null ? null : 'Catalog unavailable; manual configuration is available.',
      onRetryCatalog: () => { if (owner) void catalogs[index].refetch(); },
    }))}
    onSave={!owner ? null : async (provider, value) => {
      try { await save(workerModelDefaultsPatch(provider, value)); return { ok: true }; }
      catch (error: unknown) {
        const failure = readWriteFailure(error, SETTINGS_FAILURES, SETTINGS_TEXT);
        return { ok: false, message: failure.is === 'refused'
          ? `${SETTINGS_TEXT.refused} ${failure.text}` : SETTINGS_TEXT.unknown };
      }
    }} />;
}
