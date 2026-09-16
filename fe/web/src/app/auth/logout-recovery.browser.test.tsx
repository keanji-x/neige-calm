import { act, screen } from '@testing-library/react';
import { page } from 'vitest/browser';
import { afterEach, expect, it, vi } from 'vitest';
import '../../styles/entry.css';
import { logoutMarkerKey } from '../../../../core/domain/recovery/context.ts';
import { mountProductionApp } from './production-app.tsx';

const mounted = vi.hoisted(() => ({ roots: [] as { unmount(): void }[] }));
vi.mock('react-dom/client', async importOriginal => {
  const actual = await importOriginal<typeof import('react-dom/client')>();
  return { ...actual, createRoot: (...args: Parameters<typeof actual.createRoot>) => {
    const root = actual.createRoot(...args); mounted.roots.push(root); return root;
  } };
});
afterEach(() => { act(() => { for (const root of mounted.roots.splice(0)) root.unmount(); }); document.body.replaceChildren(); vi.unstubAllGlobals(); });
it('a logged-out cold production mount exposes explicit pairing verification inside the phone viewport', async () => {
  vi.stubGlobal('__NC_BUNDLED__', true); await page.viewport(390, 844);
  const fetch = vi.fn(); vi.stubGlobal('fetch', fetch);
  const values = new Map<string, string>([[logoutMarkerKey(), JSON.stringify({ schemaVersion: 1, fingerprint: 'a'.repeat(64) })]]);
  const storage: Storage = { get length() { return values.size; }, getItem: key => values.get(key) ?? null,
    setItem: (key, value) => { values.set(key, value); }, removeItem: key => { values.delete(key); },
    clear: () => values.clear(), key: index => [...values.keys()][index] ?? null };
  const root = document.createElement('div'); document.body.append(root);
  act(() => mountProductionApp(root, { storage, reload: vi.fn(), deleteDatabase: vi.fn() }));
  const verify = await screen.findByRole('button', { name: '验证本次配对' });
  expect(fetch).not.toHaveBeenCalled();
  const bounds = verify.getBoundingClientRect();
  expect(bounds.top).toBeGreaterThanOrEqual(0); expect(bounds.bottom).toBeLessThanOrEqual(844);
});
