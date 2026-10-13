import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { useEffect } from 'react';
import { afterEach, expect, it, vi } from 'vitest';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { DATABASE_ID_KEY, DB_INSTANCE_ID_KEY } from '../../../../core/keys/storage.ts';
import { RecoverySession } from '../../systems/recovery/session.ts';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';
import { RecoveryGate } from '../auth/recovery-gate.tsx';
import { ServerCompatGate, WEB_COMPAT_VERSION } from './public.tsx';
import { createUiPreferences, UiPreferencesProvider, useUiPreferences } from './ui-preferences.tsx';

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

it('database_id_change_overwrites_the_key', async () => {
  // The per-boot instance id is unchanged here, so none of the cache busting runs.
  const values = new Map<string, string>([[DB_INSTANCE_ID_KEY, 'boot-1'], [DATABASE_ID_KEY, 'db-old']]);
  const storage = {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
    removeItem: (key: string) => { values.delete(key); },
  };
  const reload = vi.fn();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const runtime = {
    storage, reload, deleteDatabase: () => {}, idbDatabaseName: 'test',
    fetchVersion: () => Promise.resolve({ webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION,
      syncEventVersion: 20, dbInstanceId: 'boot-1', databaseId: 'db-new' }),
  };
  const preferences = createUiPreferences(storage);
  expect(preferences.databaseScope()).toBe('db-old');
  try {
    render(<QueryClientProvider client={client}>
      <ServerCompatGate client={client} runtime={runtime} cursorStore={{ clear() {} }}>
        <UiPreferencesProvider preferences={preferences}><p>Route</p></UiPreferencesProvider>
      </ServerCompatGate>
    </QueryClientProvider>);
    await waitFor(() => expect(values.get(DATABASE_ID_KEY)).toBe('db-new'));
    await waitFor(() => expect(preferences.databaseScope()).toBe('db-new'));
    expect(reload).not.toHaveBeenCalled();
    expect(values.get(DB_INSTANCE_ID_KEY)).toBe('boot-1');
  } finally { cleanup(); client.clear(); }
});

it('leaves the database scope null, and no Area pin applied, when the kernel reports no database identity', async () => {
  const values = new Map<string, string>();
  const storage = {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
    removeItem: (key: string) => { values.delete(key); },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const runtime = {
    storage, reload: () => {}, deleteDatabase: () => {}, idbDatabaseName: 'test',
    fetchVersion: () => Promise.resolve({ webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION,
      syncEventVersion: 20, dbInstanceId: 'boot-1' }),
  };
  const preferences = createUiPreferences(storage);
  try {
    render(<QueryClientProvider client={client}>
      <ServerCompatGate client={client} runtime={runtime} cursorStore={{ clear() {} }}>
        <UiPreferencesProvider preferences={preferences}><p>Route</p></UiPreferencesProvider>
      </ServerCompatGate>
    </QueryClientProvider>);
    await waitFor(() => expect(values.get(DB_INSTANCE_ID_KEY)).toBe('boot-1'));
    expect(values.has(DATABASE_ID_KEY)).toBe(false);
    expect(preferences.databaseScope()).toBeNull();
    preferences.setAreaTrackPinned('area', 't', true);
    expect(preferences.areaTrackPinned('area', 't')).toBe(false);
  } finally { cleanup(); client.clear(); }
});

it('scopes a bundled workspace by its verified recovery database', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const values = new Map<string, string>();
  const storage = { getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
    removeItem: (key: string) => { values.delete(key); } };
  const preferences = createUiPreferences(storage);
  const access = new RecoveryAccess();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const version = { webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION,
    syncEventVersion: 20, dbInstanceId: 'bundled-boot', databaseId: 'bundled-db' };
  const fetchVersion = () => Promise.resolve(version);
  let scope = '';
  const recovery = new RecoverySession({ access, storage, origin: 'https://server.test',
    compatibleVersion: WEB_COMPAT_VERSION, version: fetchVersion,
    identity: () => Promise.resolve({ userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 'session' }),
    adoptScope: value => { scope = value; preferences.setRecoveryScope(value); },
    logout: () => Promise.resolve(), clear: () => client.clear(), online: () => true, visible: () => true });
  const transport = createRecoveryTransports({ send: () => Promise.reject(new Error('Unexpected business request')) }, access).business;
  function Reader() {
    const reader = useUiPreferences();
    const scoped = reader.databaseScope() !== null;
    useEffect(() => { if (scoped) reader.setAreaTrackPinned('area', 'visible', true); }, [reader, scoped]);
    return <p>Visible bundled track</p>;
  }
  const mounted = render(<RecoveryGate recovery={recovery} transport={transport}
    unauthorized={createUnauthorizedChannel({ enqueue: task => task() })} client={client}
    runtime={{ storage, fetchVersion, reload() {}, deleteDatabase() {}, idbDatabaseName: 'test' }}
    cursorStore={{ clear() {} }} renderLogin={() => <p>Login</p>}>
    <UiPreferencesProvider preferences={preferences}><Reader /></UiPreferencesProvider>
  </RecoveryGate>);
  try {
    await screen.findByText('Visible bundled track');
    await waitFor(() => expect(preferences.databaseScope()).toBe('bundled-db'));
    const restored = createUiPreferences(storage);
    restored.setRecoveryScope(scope); restored.setDatabaseScope('bundled-db');
    expect(restored.areaTrackPinned('area', 'visible')).toBe(true);
  } finally { mounted.unmount(); client.clear(); }
});
