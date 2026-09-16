import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';
import { DB_INSTANCE_ID_KEY } from '../../../../core/keys/storage.ts';
import { ServerCompatGate, WEB_COMPAT_VERSION, type ServerVersionInfo } from './public.tsx';
import { createUiPreferences, UiPreferencesProvider, useReadReceipt } from './ui-preferences.tsx';

afterEach(cleanup);

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
        syncEventVersion: 20, dbInstanceId: 'real-db' });
      await version;
    });
    await waitFor(() => expect(values.get(DB_INSTANCE_ID_KEY)).toBe('real-db'));
    await waitFor(() => expect(preferences.isUnread('track', 'current', 100)).toBe(false));
    expect(createUiPreferences(storage).isUnread('track', 'current', 100)).toBe(false);
  } finally { client.clear(); }
});
