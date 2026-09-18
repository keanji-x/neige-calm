import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it, vi } from 'vitest';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { RecoveryAccess } from '../../../../core/domain/recovery/access.ts';
import { DATABASE_ID_KEY, DB_INSTANCE_ID_KEY } from '../../../../core/keys/storage.ts';
import { RecoverySession } from '../../systems/recovery/session.ts';
import { createRecoveryTransports } from '../../systems/recovery/transport.ts';
import { RecoveryGate } from '../auth/recovery-gate.tsx';
import { ServerCompatGate, WEB_COMPAT_VERSION, type ServerVersionInfo } from './public.tsx';
import { createUiPreferences, UiPreferencesProvider, useReadReceipt } from './ui-preferences.tsx';

afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

it('records the initially visible track under the first discovered instance', async () => {
  const values = new Map<string, string>();
  const storage = {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
    removeItem: (key: string) => { values.delete(key); },
  };
  const preferences = createUiPreferences(storage);
  let release!: (value: ServerVersionInfo) => void;
  const version = new Promise<ServerVersionInfo>(resolve => { release = resolve; });
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const runtime = { storage, fetchVersion: () => version, reload: () => {}, deleteDatabase: () => {}, idbDatabaseName: 'test' };
  function Reader() {
    useReadReceipt('track', 'current', 100);
    return <p>Visible track</p>;
  }
  try {
    render(<QueryClientProvider client={client}>
      <ServerCompatGate client={client} runtime={runtime} cursorStore={{ clear() {} }}>
        <UiPreferencesProvider preferences={preferences}><Reader /></UiPreferencesProvider>
      </ServerCompatGate>
    </QueryClientProvider>);
    expect(screen.getByText('Visible track')).toBeTruthy();
    expect(preferences.isUnread('track', 'current', 100)).toBe(false);
    expect(values.size).toBe(0);
    await act(async () => {
      release({ webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION,
        syncEventVersion: 20, dbInstanceId: 'boot-1', databaseId: 'real-db', nowMs: 50 });
      await version;
    });
    await waitFor(() => expect(values.get(DB_INSTANCE_ID_KEY)).toBe('boot-1'));
    await waitFor(() => expect(values.get(DATABASE_ID_KEY)).toBe('real-db'));
    await waitFor(() => expect(preferences.readScope()).toBe('real-db'));
    await waitFor(() => expect(preferences.isUnread('track', 'current', 100)).toBe(false));
    // The receipt is keyed by the database identity, so the next load (which
    // seeds its scope from `DATABASE_ID_KEY`) reads it back; the activity at
    // 100 is newer than the 50 baseline, so it is the receipt, not the
    // baseline, that answers here.
    expect(createUiPreferences(storage).isUnread('track', 'current', 100)).toBe(false);
    expect(createUiPreferences(storage).isUnread('track', 'other', 100)).toBe(true);
  } finally { client.clear(); }
});

it('database_id_change_overwrites_the_key', async () => {
  /*
   * #1722 §5.2 — the stable id is overwritten when it changes, not only written
   * when missing: a database reset mints a new identity, and a device pinned to
   * the old one would seed the wrong scope on every later load. The per-boot
   * instance id is unchanged here, so none of the cache busting runs.
   */
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
      syncEventVersion: 20, dbInstanceId: 'boot-1', databaseId: 'db-new', nowMs: 50 }),
  };
  const preferences = createUiPreferences(storage);
  expect(preferences.readScope()).toBe('db-old');
  try {
    render(<QueryClientProvider client={client}>
      <ServerCompatGate client={client} runtime={runtime} cursorStore={{ clear() {} }}>
        <UiPreferencesProvider preferences={preferences}><p>Route</p></UiPreferencesProvider>
      </ServerCompatGate>
    </QueryClientProvider>);
    await waitFor(() => expect(values.get(DATABASE_ID_KEY)).toBe('db-new'));
    await waitFor(() => expect(preferences.readScope()).toBe('db-new'));
    expect(reload).not.toHaveBeenCalled();
    expect(values.get(DB_INSTANCE_ID_KEY)).toBe('boot-1');
  } finally { cleanup(); client.clear(); }
});

it('leaves the receipt scope null, and nothing unread, when the kernel reports no database identity', async () => {
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
    expect(preferences.readScope()).toBeNull();
    expect(preferences.isUnread('track', 't', Number.MAX_SAFE_INTEGER)).toBe(false);
  } finally { cleanup(); client.clear(); }
});

it('persists the initially visible bundled track under its verified recovery database', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true);
  const values = new Map<string, string>();
  const storage = { getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
    removeItem: (key: string) => { values.delete(key); } };
  const preferences = createUiPreferences(storage);
  const access = new RecoveryAccess();
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const version = { webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION,
    syncEventVersion: 20, dbInstanceId: 'bundled-boot', databaseId: 'bundled-db', nowMs: 50 };
  const fetchVersion = () => Promise.resolve(version);
  let scope = '';
  const recovery = new RecoverySession({ access, storage, origin: 'https://server.test',
    compatibleVersion: WEB_COMPAT_VERSION, version: fetchVersion,
    identity: () => Promise.resolve({ userId: 'owner', displayName: 'Owner', role: 'owner', sessionId: 'session' }),
    adoptScope: value => { scope = value; preferences.setRecoveryScope(value); },
    logout: () => Promise.resolve(), clear: () => client.clear(), online: () => true, visible: () => true });
  const transport = createRecoveryTransports({ send: () => Promise.reject(new Error('Unexpected business request')) }, access).business;
  function Reader() { useReadReceipt('track', 'visible', 100); return <p>Visible bundled track</p>; }
  const mounted = render(<RecoveryGate recovery={recovery} transport={transport}
    unauthorized={createUnauthorizedChannel({ enqueue: task => task() })} client={client}
    runtime={{ storage, fetchVersion, reload() {}, deleteDatabase() {}, idbDatabaseName: 'test' }}
    cursorStore={{ clear() {} }} renderLogin={() => <p>Login</p>}>
    <UiPreferencesProvider preferences={preferences}><Reader /></UiPreferencesProvider>
  </RecoveryGate>);
  try {
    await screen.findByText('Visible bundled track');
    await waitFor(() => expect(preferences.readScope()).toBe('bundled-db'));
    expect(preferences.isUnread('track', 'visible', 100)).toBe(false);
    const restored = createUiPreferences(storage);
    restored.setRecoveryScope(scope); restored.setReadScope('bundled-db', 50);
    expect(restored.isUnread('track', 'visible', 100)).toBe(false);
  } finally { mounted.unmount(); client.clear(); }
});
