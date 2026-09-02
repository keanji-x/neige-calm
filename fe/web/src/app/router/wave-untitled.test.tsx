// @vitest-environment jsdom
//
// #1211 S2 — a wave that starts with no name and no words in it, driven
// through the real router, the real shell dialog and the real transport port.
//
// Two things had to be built for that to be a usable product rather than a
// blank page, and both are wiring that no single component can be asked about:
//
//   1. **Creating lands in the spec conversation, with the caret in it.** The
//      shell cannot name the card to open — `POST /api/waves` answers with a
//      `Wave` — so it marks the *navigation* it makes, and `WaveRouteBody`
//      redeems that mark against its own cards. That hand-off spans three
//      modules, which is exactly why it is asserted here and not in any of
//      them.
//   2. **Clearing the title is a request, not a cancel.** The wave header
//      passes `emptyCommit="clear"` so the spec agent's `calm.wave.rename` can
//      name the wave again; what proves it is the PATCH on the wire.

import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { act, cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

const COVE = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
/* The wave the create answers with. `title: ''` is what the kernel stores when
   the POST omits the key, which is the whole point of the slice. */
const WAVE = {
  id: 'w1', cove_id: 'c1', title: '', sort: 1, lifecycle: 'draft', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2,
};
/* A named wave, for the rename half: clearing a name that is already empty is
   the arithmetic no-op, so the PATCH case needs something to clear. */
const NAMED_WAVE = { ...WAVE, title: 'Test wave' };
/* The wave a *second* create answers with. It exists so the intent can be
   stated while the reader is standing on some other wave — the rail's per-cove
   `+` is rendered by `AppShell`, above the route outlet, so "read one wave and
   start another" is an ordinary move and the wave being left is still
   mounted. */
const OTHER_WAVE = { ...WAVE, id: 'w2', sort: 2, created_at: 3, updated_at: 3 };
const SPEC_CARD = {
  id: 'card-spec', wave_id: 'w1', kind: 'codex', title: 'Spec chat', sort: 1,
  payload: { spec_harness: true }, deletable: true, created_at: 1, updated_at: 2,
};
const OTHER_SPEC_CARD = { ...SPEC_CARD, id: 'card-spec-w2', wave_id: 'w2' };

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

type Options = {
  wave?: typeof WAVE;
  cards?: readonly unknown[];
  /** What `POST /api/waves` answers with. Defaults to the wave already listed,
   *  which is the single-wave shape most of these cases want. */
  created?: typeof WAVE;
  createdCards?: readonly unknown[];
  /** Start with the created wave's detail failing, so the reader lands on the
   *  error box and the route body never mounts. Flipped back through the
   *  returned `gate`. */
  createdDetailFails?: boolean;
};

function setup(options: Options = {}) {
  const wave = options.wave ?? WAVE;
  const cards = options.cards ?? [SPEC_CARD];
  const created = options.created ?? wave;
  const createdCards = options.createdCards ?? cards;
  const gate = { createdDetailFails: options.createdDetailFails ?? false };
  const details = new Map<string, { wave: typeof WAVE; cards: readonly unknown[] }>([
    [wave.id, { wave, cards }],
    [created.id, { wave: created, cards: createdCards }],
  ]);
  const requests: ApiRequest[] = [];
  const values = new Map<string, string>();
  const transport: ApiTransportPort = {
    send(request) {
      requests.push(request);
      if (request.path === '/api/coves') return Promise.resolve(ok([COVE]));
      if (request.path === '/api/coves/c1/waves') {
        return Promise.resolve(ok(created.id === wave.id ? [wave] : [wave, created]));
      }
      if (request.path === '/api/coves/c1/conversations') return Promise.resolve(ok([]));
      if (request.path === '/api/overlays?entity_kind=wave') return Promise.resolve(ok([]));
      if (request.path === '/api/wave-templates') return Promise.resolve(ok([]));
      if (request.method === 'POST' && request.path === '/api/waves') return Promise.resolve(ok(created));
      const patched = request.method === 'PATCH' ? details.get(request.path.slice('/api/waves/'.length)) : undefined;
      if (patched !== undefined) {
        return Promise.resolve(ok({ ...patched.wave, ...(request.body as object) }));
      }
      if (request.path.endsWith('/conversations')) return Promise.resolve(ok([]));
      const detail = details.get(request.path.slice('/api/waves/'.length));
      if (detail !== undefined) {
        if (detail.wave.id === created.id && created.id !== wave.id && gate.createdDetailFails) {
          return Promise.resolve({ status: 500, statusText: 'Server Error', body: {} });
        }
        return Promise.resolve(ok({ wave: detail.wave, cards: detail.cards, overlays: [] }));
      }
      if (request.path.endsWith('/spec/run')) {
        return Promise.resolve(ok({ card_id: SPEC_CARD.id, runtime_id: 'r', phase: 'idle' }));
      }
      if (request.path === '/api/settings') return Promise.resolve(ok({}));
      return Promise.resolve(ok([]));
    },
  };
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({
    transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: vi.fn(),
  });
  render(
    <QueryClientProvider client={client}>
      <ThemeProvider storage={{
        getItem: (key: string) => values.get(key) ?? null,
        setItem: (key: string, value: string) => { values.set(key, value); },
      }}
      >
        <RouterProvider router={router} />
      </ThemeProvider>
    </QueryClientProvider>,
  );
  return {
    requests,
    router,
    gate,
    /** Change what the next read of a wave's detail answers with. */
    setCardsOf: (waveId: string, next: readonly unknown[]) => {
      const detail = details.get(waveId);
      if (detail !== undefined) details.set(waveId, { wave: detail.wave, cards: next });
    },
    client,
  };
}

/* The composer's field. `combobox` and not `textbox`: the wave route passes
   `onNewConversation`, which arms the `/` trigger menu and turns Astryx's
   editable into a combobox. */
function messageField(): HTMLElement {
  return screen.getByRole('combobox', { name: 'Message' });
}

async function createAWave() {
  /* Exact: the rail's per-cove opener is `New wave in Work`, which a substring
     match would also find — the cove page's own `+` is the one this drives. */
  await userEvent.click(await screen.findByRole('button', { name: /^New wave$/ }));
  await screen.findByRole('dialog', { name: 'New wave' });
  await userEvent.click(await screen.findByRole('button', { name: 'Create wave' }));
}

/** The same create, started from the rail — which is reachable from *every*
 *  route, including a wave page. */
async function createAWaveFromTheRail() {
  await userEvent.click(await screen.findByRole('button', { name: 'New wave in Work' }));
  await screen.findByRole('dialog', { name: 'New wave' });
  await userEvent.click(await screen.findByRole('button', { name: 'Create wave' }));
}

async function goToWave(router: ReturnType<typeof setup>['router'], waveId: string) {
  await act(async () => { await router.navigate({ to: '/wave/$waveId', params: { waveId } }); });
}

beforeEach(() => {
  window.history.pushState({}, '', `${APP_BASEPATH}/cove/c1`);
  /* The drawer, the dialog and `EditableTitle` all move focus inside a frame.
     Running frames synchronously is what makes "who ended up with the focus"
     a question this tier can answer at all. */
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => { callback(0); return 1; });
  vi.stubGlobal('cancelAnimationFrame', vi.fn());
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe('creating a wave lands in its spec conversation', () => {
  /*
   * The landing, end to end. Nothing is typed into the dialog — there is
   * nothing to type into it any more — and what the reader gets is the spec
   * conversation open with the caret in the composer, because their first
   * sentence is the wave's intent.
   *
   * Red when: the shell stops stating the intent, `WaveRouteBody` stops
   * redeeming it, the panel drops the `focusComposer` flag on the way to the
   * composer, or `ChatComposer` stops honouring `focusOnMount`.
   */
  it('opens the spec conversation with the caret in the composer', async () => {
    setup();
    await createAWave();
    await screen.findByRole('complementary', { name: 'Spec chat' });
    await waitFor(() => { expect(document.activeElement).toBe(messageField()); });
  });

  /*
   * ── The same landing, started from a wave page ───────────────────────────
   *
   * The rail's per-cove `+` is rendered by `AppShell`, above the route outlet,
   * so it is on screen on every route — "read one wave, start another" is an
   * ordinary move, and it is the move a *global* intent slot cannot survive:
   * the wave being left is still mounted when the intent is stated, and it
   * gets a render before the route changes.
   *
   * Red when the intent can be seen, and therefore consumed or discarded, by
   * any route body other than the one the navigation is going to.
   */
  it('opens the spec conversation of the new wave when the create started on another wave', async () => {
    const { router } = setup({ created: OTHER_WAVE, createdCards: [OTHER_SPEC_CARD] });
    await goToWave(router, 'w1');
    await screen.findByRole('button', { name: 'Rename wave' });

    await createAWaveFromTheRail();

    await waitFor(() => { expect(router.state.location.pathname.endsWith('/wave/w2')).toBe(true); });
    /* The drawer lives in `WaveRouteBody`, which is keyed by wave and unmounts
       with the route — so a drawer open on this page is this wave's own. */
    await screen.findByRole('complementary', { name: 'Spec chat' });
    await waitFor(() => { expect(document.activeElement).toBe(messageField()); });
  });

  /*
   * ── An intent that never landed must not lie in wait ─────────────────────
   *
   * The detail read fails, so `WaveRoute` returns the error box and the body
   * that would redeem the intent never mounts. The reader gives up and walks
   * back to the cove. Whatever is holding "open the spec conversation" has to
   * have died with that navigation: coming back to the wave later is an
   * ordinary visit, and an ordinary visit does not spring the drawer open and
   * take the caret.
   *
   * Red when the intent outlives the navigation that stated it.
   */
  it('does not open on a later visit when the landing never reached the wave', async () => {
    const { router, gate } = setup({
      created: OTHER_WAVE, createdCards: [OTHER_SPEC_CARD], createdDetailFails: true,
    });
    await createAWave();
    await screen.findByRole('button', { name: 'Retry' });

    await act(async () => { await router.navigate({ to: '/cove/$coveId', params: { coveId: 'c1' } }); });
    await screen.findByRole('button', { name: 'Rename cove' });

    gate.createdDetailFails = false;
    await goToWave(router, 'w2');
    await screen.findByRole('button', { name: 'Rename wave' });
    expect(screen.queryByRole('complementary', { name: 'Spec chat' })).toBeNull();
  });

  /*
   * And it is a one-shot. The intent is cleared as it is redeemed, so walking
   * back into the same wave later is an ordinary visit — a reader who closed
   * the conversation must not have it forced open again every time.
   *
   * Red when the mark is not consumed — left on the entry, or read from
   * somewhere that outlives the navigation that wrote it.
   */
  it('does not re-open the conversation on a later visit to the same wave', async () => {
    const { router } = setup();
    await createAWave();
    await screen.findByRole('complementary', { name: 'Spec chat' });

    /* Leaving the route takes the drawer with it — that is the "closed" state
       this case needs, and it is one the reader reaches every time they walk
       away. Coming back is then an ordinary visit, and it must stay one. */
    await act(async () => { await router.navigate({ to: '/cove/$coveId', params: { coveId: 'c1' } }); });
    await screen.findByRole('button', { name: 'Rename cove' });
    expect(screen.queryByRole('complementary', { name: 'Spec chat' })).toBeNull();

    await act(async () => { await router.navigate({ to: '/wave/$waveId', params: { waveId: 'w1' } }); });
    await screen.findByRole('button', { name: 'Rename wave' });
    expect(screen.queryByRole('complementary', { name: 'Spec chat' })).toBeNull();
  });

  /*
   * A wave with no spec card must not leave the request standing, or the
   * drawer springs open on whichever wave the reader visits next that does
   * have one. Nothing opens, and nothing is left armed.
   */
  it('opens nothing, and arms nothing, when the wave has no spec card', async () => {
    setup({ cards: [] });
    await createAWave();
    await screen.findByRole('button', { name: 'Rename wave' });
    expect(screen.queryByRole('complementary', { name: 'Spec chat' })).toBeNull();
  });
});

describe('clearing the wave title', () => {
  /*
   * `emptyCommit="clear"` on the wave header, proved on the wire.
   *
   * The empty title is the one state `calm.wave.rename` will fill in (#1211
   * S3), so "clear the name" is how a reader hands naming back to the agent.
   * Swallowing the keystroke — which is what the primitive does by default,
   * and still does for a cove — would leave "I cleared it, pressed Enter, and
   * nothing happened".
   *
   * Red when the wave page drops `emptyCommit`, or the primitive stops
   * honouring it.
   */
  it('PATCHes an empty title when the box is emptied and committed', async () => {
    const { requests, router } = setup({ wave: NAMED_WAVE });
    await act(async () => { await router.navigate({ to: '/wave/$waveId', params: { waveId: 'w1' } }); });
    await userEvent.click(await screen.findByRole('button', { name: 'Rename wave' }));
    await userEvent.clear(screen.getByRole('textbox', { name: 'Wave title' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Wave title' }), '{Enter}');

    await waitFor(() => {
      expect(requests.filter((request) => request.method === 'PATCH' && request.path === '/api/waves/w1'))
        .toEqual([expect.objectContaining({ body: { title: '' } })]);
    });
  });
});
