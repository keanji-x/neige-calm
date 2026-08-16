// @vitest-environment jsdom
//
// `INV-CARD-226` at the only place it is user-visible: the wave route's CARDS
// panel. The partition helper has its own unit tests, but a helper nobody calls
// is a no-op — these drive the real route, the real registry and the real
// built-ins, so unwiring `cards={panelCards}` back to `cards={cards}` is red.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import type { CardWire } from '../../../../core/domain/wave.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import type { CardEntry } from '../../systems/cards/public.js';
import { ThemeProvider } from '../theme/public.tsx';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const COVE = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const WAVE = {
  id: 'w1', cove_id: 'c1', title: 'Test wave', sort: 1, lifecycle: 'working', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2,
};
const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

function card(overrides: Partial<CardWire> & Pick<CardWire, 'id' | 'kind'>): CardWire {
  return {
    wave_id: WAVE.id, title: null, sort: 1, payload: {}, deletable: true,
    created_at: 1, updated_at: 2, ...overrides,
  };
}

/*
 * Wire order is the assertion: the panel must present exactly the surviving
 * cards, in the order the kernel sent them, with the two headless kinds gone.
 * The headless pair is interleaved on purpose — dropping them must not shuffle
 * what is left.
 */
const SPEC_CARD = card({ id: 'card-spec', kind: 'codex', title: 'Spec chat', payload: { spec_harness: true }, sort: 1 });
const UNKNOWN_TERMINAL = card({ id: 'card-term', kind: 'terminal', title: 'Terminal one', sort: 2 });
const REPORT_CARD = card({ id: 'card-report', kind: 'wave-report', title: 'Report card', sort: 3, payload: { body: '' } });
const VISIBLE_CARD = card({ id: 'card-surface', kind: 'panel-surface', title: 'Surface', sort: 4 });
const ORDINARY_CODEX = card({ id: 'card-codex', kind: 'codex', title: 'Codex chat', sort: 5, payload: {} });
const CARDS = [SPEC_CARD, UNKNOWN_TERMINAL, REPORT_CARD, VISIBLE_CARD, ORDINARY_CODEX];

/*
 * S1 lands no card with a surface, so the visible branch of the partition has
 * no production entry to exercise yet. This fixture is the *only* stub here:
 * everything else — registry, built-ins, route, panel — is production code.
 * Without it the test could not tell "kept every non-headless card" apart from
 * "kept only the cards no adapter claimed".
 *
 * The type is deliberately **not** a member of `BUILTIN_CARD_ORDER`. Registry
 * registration is keyed by type and overwrites, and this runs after
 * `bootTestCardRuntime()`, so naming it e.g. `file-viewer` would silently
 * shadow that entry the day S3c/the viewer epic lands it — the test would keep
 * exercising the stub with no signal at all. `headless-filter.test.ts` keeps
 * `surface-fixture` out of the tuple for the same reason.
 *
 * It also actually renders something. The whole point of the fixture is "a card
 * that owns a surface"; a `() => null` component would be the same shape as the
 * headless entries it is here to be distinguished from.
 */
type SurfaceFixtureCard = Readonly<{ type: 'panel-surface-fixture'; id: string }>;
const SURFACE_FIXTURE_ENTRY: CardEntry<SurfaceFixtureCard> = {
  type: 'panel-surface-fixture',
  component: ({ card: value }) => <div>{`surface for ${value.id}`}</div>,
  defaultSize: { w: 4, h: 6, minW: 3, minH: 3 },
  title: () => 'Surface',
  accessibleName: () => 'Surface',
  create: { mode: 'kernel-minted-only' },
  fromKernel: (raw) => (raw.kind === 'panel-surface' ? { type: 'panel-surface-fixture', id: raw.id } : null),
};

function setup(cards: readonly CardWire[] = CARDS, { withVisibleFixture = true } = {}) {
  const themeValues = new Map<string, string>();
  const themeStorage: Pick<Storage, 'getItem' | 'setItem'> = {
    getItem: (key) => themeValues.get(key) ?? null,
    setItem: (key, value) => { themeValues.set(key, value); },
  };
  const ok = (body: unknown): ApiTransportResponse => ({ status: 200, statusText: 'OK', body });
  const transport: ApiTransportPort = {
    send(request) {
      if (request.path === '/api/coves') return Promise.resolve(ok([COVE]));
      if (request.path === '/api/coves/c1/waves') return Promise.resolve(ok([WAVE]));
      if (request.path === '/api/overlays?entity_kind=wave') return Promise.resolve(ok([]));
      if (request.path === '/api/waves/w1') {
        return Promise.resolve(ok({ wave: WAVE, cards: [...cards], overlays: [] }));
      }
      if (request.path === '/api/settings') return Promise.resolve(ok({}));
      return Promise.resolve(ok([]));
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, structuralSharing: false } } });
  const runtime = bootTestCardRuntime();
  if (withVisibleFixture) runtime.registry.register(SURFACE_FIXTURE_ENTRY as unknown as CardEntry);
  const router = createAppRouter({ transport, unauthorized, client, cards: runtime, onSignOut: vi.fn() });
  render(<QueryClientProvider client={client}><ThemeProvider storage={themeStorage}>
    <RouterProvider router={router} />
  </ThemeProvider></QueryClientProvider>);
  return { runtime };
}

async function inventoryLabels(): Promise<string[]> {
  // `[data-nc-card-inventory]` is the CARDS module's own list, not any of the
  // page's other lists — the panel is what the user reads.
  const list = await waitFor(() => {
    const found = document.querySelector('[data-nc-card-inventory]');
    if (found === null) throw new Error('card inventory has not rendered');
    return found as HTMLElement;
  });
  return within(list).getAllByRole('listitem').map((row) => row.textContent ?? '');
}

beforeEach(() => {
  window.history.pushState({}, '', `${APP_BASEPATH}/wave/w1`);
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('wave route CARDS panel', () => {
  it('[INV-CARD-226] drops the resolved headless cards from the rendered panel', async () => {
    setup();
    const labels = await inventoryLabels();
    // The two headless kinds are gone from the product surface, not merely
    // absent from a helper's return value.
    expect(labels.some((label) => label.includes('Spec chat'))).toBe(false);
    expect(labels.some((label) => label.includes('Report card'))).toBe(false);
    // The spec card is still on the page as a *conversation* — it is the CARDS
    // module it must be absent from, so the assertion stays scoped to it.
    expect(screen.queryByRole('button', { name: /Conversation Spec chat/ })).toBeTruthy();
  });

  it('[INV-CARD-226] keeps unclaimed cards, because an unlisted card is worse than an unrecognised one', async () => {
    setup();
    const labels = await inventoryLabels();
    // Ordinary codex and terminal have no adapter in S1. They are unknown, not
    // headless, and hiding them would lose real cards from the product.
    expect(labels.some((label) => label.includes('Codex chat'))).toBe(true);
    expect(labels.some((label) => label.includes('Terminal one'))).toBe(true);
  });

  it('[INV-CARD-226] renders exactly the surviving cards in the kernel wire order', async () => {
    setup();
    expect(await inventoryLabels()).toEqual(['Terminal one', 'Surface', 'Codex chat']);
  });

  it('[INV-CARD-226] keeps a card with a surface, so the filter is headless-only and not adapter-only', async () => {
    const { runtime } = setup();
    expect(runtime.registry.resolve({ id: VISIBLE_CARD.id, kind: 'panel-surface', payload: {} })?.type)
      .toBe('panel-surface-fixture');
    expect(await inventoryLabels()).toContain('Surface');
  });

  it('[INV-CARD-226] shows the empty state when every card the wave has is headless', async () => {
    setup([SPEC_CARD, REPORT_CARD], { withVisibleFixture: false });
    expect(await screen.findByText('No cards yet.')).toBeTruthy();
  });
});
