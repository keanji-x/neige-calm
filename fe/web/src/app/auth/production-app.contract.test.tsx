// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from 'vitest';
import { mountProductionApp } from './production-app.tsx';
import { createAppRouter } from '../router/public.tsx';
import { logoutOperation, runOperation } from '../providers/queries.ts';
import { bootCards } from '../cards.ts';

const mocks = vi.hoisted(() => ({ render: vi.fn() }));

vi.mock('react-dom/client', () => ({ createRoot: vi.fn(() => ({ render: mocks.render })) }));
vi.mock('../composition.ts', () => ({ createBrowserEventComposition: vi.fn(() => ({
  store: { clear: vi.fn() }, stream: {},
})) }));
vi.mock('../providers/transport.ts', () => ({ createFetchTransport: vi.fn(() => ({ send: vi.fn() })) }));
vi.mock('../router/public.tsx', () => ({ createAppRouter: vi.fn(() => ({})) }));
vi.mock('../cards.ts', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../cards.ts')>();
  return { bootCards: vi.fn(actual.bootCards) };
});
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

  it('assembles the card runtime exactly once and injects that instance into the router', () => {
    const root = document.createElement('div');
    const storage = {
      length: 0, clear: vi.fn(), getItem: vi.fn(() => null), key: vi.fn(() => null),
      removeItem: vi.fn(), setItem: vi.fn(),
    } satisfies Storage;

    mountProductionApp(root, { storage, reload: vi.fn(), deleteDatabase: vi.fn() });

    // One mount, one boot. There is no module-level registry and no module-level
    // "already registered" guard to fall back on (`INV-CARD-224` is retired), so
    // a second boot call here would mean a second, differently-populated
    // registry somewhere in the app.
    expect(bootCards).toHaveBeenCalledOnce();
    const cards = vi.mocked(createAppRouter).mock.calls[0]?.[0]?.cards;
    expect(cards).toBeDefined();
    // The booted registry is the one the router got — not a second instance.
    expect(vi.mocked(bootCards).mock.calls[0]?.[0]).toBe(cards?.registry);
    expect(cards?.registry.entries().map((entry) => entry.type))
      .toEqual(['terminal', 'codex', 'spec', 'claude', 'wave-report', 'file-viewer']);
    expect(cards?.host).toBeDefined();
    /* The host is built with the filesystem port, not bare: a card mounted on a
       host without it renders "this board was built without filesystem access",
       which is the shape of the defect this line exists to catch. */
    expect(cards?.host.mount({ type: 'terminal', id: 'probe', title: null, terminalId: null }).card.files)
      .not.toBeNull();
  });

  it('gives each mount its own registry instead of sharing module state', () => {
    const storage = {
      length: 0, clear: vi.fn(), getItem: vi.fn(() => null), key: vi.fn(() => null),
      removeItem: vi.fn(), setItem: vi.fn(),
    } satisfies Storage;
    const browser = { storage, reload: vi.fn(), deleteDatabase: vi.fn() };

    mountProductionApp(document.createElement('div'), browser);
    mountProductionApp(document.createElement('div'), browser);

    expect(bootCards).toHaveBeenCalledTimes(2);
    const [first, second] = vi.mocked(createAppRouter).mock.calls.map((call) => call[0]?.cards.registry);
    expect(first).not.toBe(second);
  });
});
