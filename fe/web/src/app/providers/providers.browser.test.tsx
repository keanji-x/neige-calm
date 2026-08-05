import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { cleanup, render } from '@testing-library/react';
import { page, userEvent } from 'vitest/browser';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { RefreshRequiredOverlay, ServerCompatGate, WEB_COMPAT_VERSION, type ProviderRuntime } from './public.tsx';

const incompatible = { webCompatVersion: 9, minWebCompatVersion: WEB_COMPAT_VERSION + 1, syncEventVersion: 2, dbInstanceId: 'db-a' };
const reload = vi.fn();
const values = new Map<string, string>();
const storage = { getItem: (key: string) => values.get(key) ?? null, setItem: (key: string, value: string) => { values.set(key, value); }, removeItem: (key: string) => { values.delete(key); } };
const runtime: ProviderRuntime = { fetchVersion: () => Promise.resolve(incompatible), reload, deleteDatabase: vi.fn(), idbDatabaseName: 'neige-calm', storage };
afterEach(() => { cleanup(); reload.mockClear(); values.clear(); });

describe('browser provider contracts', () => {
  it('CAP-APP-006 hard-covers the tree, reports both versions, and captures hit testing', async () => {
    const client = new QueryClient(); render(<QueryClientProvider client={client}><ServerCompatGate client={client} runtime={runtime}><button data-testid="under">under</button></ServerCompatGate></QueryClientProvider>);
    const overlay = page.getByTestId('refresh-overlay'); await expect.element(overlay).toBeVisible();
    const dialog = page.getByRole('dialog');
    await expect.element(dialog).toHaveTextContent(`compat v${incompatible.minWebCompatVersion}`); await expect.element(dialog).toHaveTextContent(`compat v${WEB_COMPAT_VERSION}`);
    const box = overlay.element().getBoundingClientRect(); expect(box.left).toBe(0); expect(box.top).toBe(0); expect(box.right).toBe(innerWidth); expect(box.bottom).toBe(innerHeight);
    expect(document.elementFromPoint(2, 2)?.closest('[data-nc-refresh-overlay]')).not.toBeNull();
    expect(document.querySelector('[data-testid="under"]')).toBeNull();
  });

  it('INV-APP-009 routes button, Escape, and backdrop through the same reload handler', async () => {
    const view = render(<RefreshRequiredOverlay server={incompatible} reload={reload} />);
    await userEvent.click(page.getByRole('button', { name: 'Refresh now' })); expect(reload).toHaveBeenCalledTimes(1);
    await userEvent.keyboard('{Escape}'); expect(reload).toHaveBeenCalledTimes(2);
    await userEvent.click(page.getByTestId('refresh-overlay'), { position: { x: 2, y: 2 } }); expect(reload).toHaveBeenCalledTimes(3); view.unmount();
  });

});
