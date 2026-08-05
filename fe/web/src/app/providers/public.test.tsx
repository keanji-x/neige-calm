// @vitest-environment jsdom
import { QueryClient } from '@tanstack/react-query';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { DB_INSTANCE_ID_KEY, SYNC_CURSOR_KEY } from '../../../../core/keys/storage.ts';
import { AppProviders, ServerCompatGate, WEB_COMPAT_VERSION, type ProviderRuntime } from './public.tsx';
import { useTheme as requireTheme } from '../theme/public.tsx';

const compatible = { webCompatVersion: WEB_COMPAT_VERSION, minWebCompatVersion: WEB_COMPAT_VERSION, syncEventVersion: 2, dbInstanceId: 'db-a' };
function memoryStorage() { const values = new Map<string, string>(); return { values, getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); }, removeItem: (key: string) => { values.delete(key); } }; }
function runtime(overrides: Partial<ProviderRuntime> = {}): ProviderRuntime {
  return { fetchVersion: () => Promise.resolve(compatible), reload: vi.fn(), deleteDatabase: vi.fn(), idbDatabaseName: 'neige-calm', storage: memoryStorage(), ...overrides };
}
afterEach(() => { cleanup(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

describe('provider behavior', () => {
  it('INV-APP-003 places one ThemeProvider above the complete children tree', () => {
    function ThemeConsumer() {
      const { resolved } = requireTheme();
      return <span>route:{resolved}</span>;
    }
    render(<AppProviders client={new QueryClient()} runtime={runtime()}><ThemeConsumer /></AppProviders>);
    expect(screen.getByText(/^route:(?:light|dark)$/u)).toBeTruthy();
  });
  it('CAP-APP-006 initially focuses the refresh action and exposes exactly one focusable exit inside the panel', async () => {
    vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
    vi.stubGlobal('cancelAnimationFrame', vi.fn());
    const server = { ...compatible, minWebCompatVersion: WEB_COMPAT_VERSION + 1 };
    render(<ServerCompatGate client={new QueryClient()} runtime={runtime({ fetchVersion: () => Promise.resolve(server) })}>route</ServerCompatGate>);
    const action = await screen.findByRole('button', { name: 'Refresh now' });
    expect(document.activeElement).toBe(action);
    const panel = screen.getByRole('dialog');
    expect(panel.querySelectorAll('a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])')).toHaveLength(1);
  });
  it('INV-APP-001 INV-APP-002 mounts the bridge only after a compatible verdict while children remain', async () => {
    let resolve!: (value: typeof compatible) => void;
    const fetchVersion = () => new Promise<typeof compatible>((done) => { resolve = done; });
    render(<AppProviders client={new QueryClient()} runtime={runtime({ fetchVersion })} renderEventBridge={() => <i>bridge</i>}><b>route</b></AppProviders>);
    expect(screen.getByText('route')).toBeTruthy(); expect(screen.queryByText('bridge')).toBeNull();
    resolve(compatible); await waitFor(() => expect(screen.getByText('bridge')).toBeTruthy());
  });

  it('INV-APP-007 checks once per mount without retaining the version query', async () => {
    const fetchVersion = vi.fn(() => Promise.resolve(compatible)); const first = render(<AppProviders client={new QueryClient()} runtime={runtime({ fetchVersion })}>ok</AppProviders>);
    await waitFor(() => expect(fetchVersion).toHaveBeenCalledTimes(1)); first.unmount();
    render(<AppProviders client={new QueryClient()} runtime={runtime({ fetchVersion })}>ok</AppProviders>);
    await waitFor(() => expect(fetchVersion).toHaveBeenCalledTimes(2));
  });

  it('INV-APP-008 preserves children and skips cache work on a version failure', async () => {
    const clear = vi.fn(); const client = new QueryClient(); vi.spyOn(client, 'clear').mockImplementation(clear);
    render(<ServerCompatGate client={client} runtime={runtime({ fetchVersion: () => Promise.reject(new Error('offline')) })}>route</ServerCompatGate>);
    expect(screen.getByText('route')).toBeTruthy(); await new Promise((done) => setTimeout(done, 0)); expect(clear).not.toHaveBeenCalled();
  });

  it('CAP-APP-010 INV-APP-011 INV-APP-014 silently busts all artifacts and hides children', async () => {
    const storage = memoryStorage(); storage.setItem(DB_INSTANCE_ID_KEY, 'db-old'); storage.setItem(SYNC_CURSOR_KEY, 'cursor');
    const clear = vi.fn(); const client = new QueryClient(); vi.spyOn(client, 'clear').mockImplementation(clear);
    const reload = vi.fn(); const deleteDatabase = vi.fn(); const r = runtime({ storage, reload, deleteDatabase }); const confirm = vi.spyOn(window, 'confirm');
    render(<ServerCompatGate client={client} runtime={r}>route</ServerCompatGate>);
    await waitFor(() => expect(reload).toHaveBeenCalledOnce());
    expect(clear).toHaveBeenCalledOnce(); expect(deleteDatabase).toHaveBeenCalledWith('neige-calm');
    expect(storage.getItem(SYNC_CURSOR_KEY)).toBeNull(); expect(storage.getItem(DB_INSTANCE_ID_KEY)).toBe('db-a');
    expect(confirm).not.toHaveBeenCalled(); expect(screen.queryByText('route')).toBeNull();
  });

  it('INV-APP-012 INV-APP-013 preserves matching artifacts and first visit only records the id', async () => {
    for (const prior of [null, 'db-a']) {
      const storage = memoryStorage(); if (prior) storage.setItem(DB_INSTANCE_ID_KEY, prior); storage.setItem(SYNC_CURSOR_KEY, 'keep');
      const reload = vi.fn(); const deleteDatabase = vi.fn(); const r = runtime({ storage, reload, deleteDatabase }); const client = new QueryClient(); const clear = vi.spyOn(client, 'clear');
      const view = render(<ServerCompatGate client={client} runtime={r}>route</ServerCompatGate>);
      await waitFor(() => expect(storage.getItem(DB_INSTANCE_ID_KEY)).toBe('db-a'));
      expect(clear).not.toHaveBeenCalled(); expect(deleteDatabase).not.toHaveBeenCalled(); expect(reload).not.toHaveBeenCalled();
      expect(storage.getItem(SYNC_CURSOR_KEY)).toBe('keep'); view.unmount();
    }
  });

  it('INV-APP-015 degrades when storage and indexedDB adapters throw', async () => {
    const broken = { getItem: () => { throw new Error('blocked'); }, setItem: () => { throw new Error('blocked'); }, removeItem: () => { throw new Error('blocked'); }, clear: vi.fn(), key: () => null, length: 0 } satisfies Storage;
    render(<AppProviders client={new QueryClient()} runtime={runtime({ storage: broken, deleteDatabase: () => { throw new Error('blocked'); } })}>route</AppProviders>);
    expect(screen.getByText('route')).toBeTruthy(); await new Promise((done) => setTimeout(done, 0)); expect(screen.getByText('route')).toBeTruthy();
  });
});
