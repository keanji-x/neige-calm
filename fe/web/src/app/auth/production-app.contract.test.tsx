// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { mountProductionApp } from './production-app.tsx';
import { createAppRouter } from '../router/public.tsx';
import { logoutOperation, runOperation } from '../providers/queries.ts';

const mocks = vi.hoisted(() => ({ render: vi.fn() }));

vi.mock('react-dom/client', () => ({ createRoot: vi.fn(() => ({ render: mocks.render })) }));
vi.mock('../composition.ts', () => ({ createBrowserEventComposition: vi.fn(() => ({
  store: { clear: vi.fn() }, stream: {},
})) }));
vi.mock('../providers/transport.ts', () => ({ createFetchTransport: vi.fn(() => ({ send: vi.fn() })) }));
vi.mock('../router/public.tsx', () => ({ createAppRouter: vi.fn(() => ({})) }));
vi.mock('../providers/queries.ts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../providers/queries.ts')>();
  return { ...actual, logoutOperation: vi.fn(() => ({ method: 'POST', path: '/logout' })), runOperation: vi.fn() };
});

afterEach(() => vi.clearAllMocks());

describe('production app mount', () => {
  it('wires sign-out through logout completion and then reloads the browser', async () => {
    let finishLogout!: () => void;
    vi.mocked(runOperation).mockReturnValue(new Promise<void>((resolve) => { finishLogout = resolve; }));
    const reload = vi.fn();
    const root = document.createElement('div');
    const storage = {
      length: 0, clear: vi.fn(), getItem: vi.fn(() => null), key: vi.fn(() => null),
      removeItem: vi.fn(), setItem: vi.fn(),
    } satisfies Storage;

    mountProductionApp(root, {
      storage,
      reload,
      deleteDatabase: vi.fn(),
    });
    const routerOptions = vi.mocked(createAppRouter).mock.calls[0]?.[0];
    expect(routerOptions).toBeDefined();

    routerOptions?.onSignOut();
    expect(logoutOperation).toHaveBeenCalledOnce();
    expect(runOperation).toHaveBeenCalledOnce();
    expect(reload).not.toHaveBeenCalled();

    finishLogout();
    await vi.waitFor(() => expect(reload).toHaveBeenCalledOnce());
  });
});
