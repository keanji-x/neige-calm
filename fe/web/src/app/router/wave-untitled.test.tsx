// @vitest-environment jsdom
//
// #1211 S2 — a wave that starts with no name and no words in it, driven
// through the real router, the real composer route and the real transport port.
//
// Two things had to be built for that to be a usable product rather than a
// blank page, and both are wiring that no single component can be asked about:
//
//   1. **Creating lands in the spec conversation, with the caret in it.** The
//      create site cannot name the card to open — `POST /api/waves` answers
//      with a `Wave` — so it marks the *navigation* it makes, and
//      `WaveRouteBody` redeems that mark against its own cards. That hand-off
//      spans three modules, which is exactly why it is asserted here and not in
//      any of them. Since #1211 S3 the create site is `/area/{id}/new` rather
//      than a dialog; the hand-off is the same one and this file drives it
//      through the page.
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

const AREA = { id: 'c1', name: 'Work', color: '#000', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
/* The wave the create answers with. `title: ''` is what the kernel stores when
   the POST omits the key, which is the whole point of the slice. */
const WAVE = {
  id: 'w1', area_id: 'c1', title: '', sort: 1, lifecycle: 'draft', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2,
};
/* A named wave, for the rename half: clearing a name that is already empty is
   the arithmetic no-op, so the PATCH case needs something to clear. */
const NAMED_WAVE = { ...WAVE, title: 'Test wave' };
/* The wave a *second* create answers with. It exists so the intent can be
   stated while the reader is standing on some other wave — the rail's per-area
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
      if (request.path === '/api/areas') return Promise.resolve(ok([AREA]));
      if (request.path === '/api/areas/c1/waves') {
        return Promise.resolve(ok(created.id === wave.id ? [wave] : [wave, created]));
      }
      if (request.path === '/api/areas/c1/conversations') return Promise.resolve(ok([]));
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

/* The composer page, with the one thing it does ask for typed into it.
   Since #1211 S3 the `+` navigates to `/area/{id}/new` instead of opening a
   dialog, so a create waits for the page's own field rather than
   `role="dialog"`, and Create stays disabled until that field says something.
   What is typed is the wave's **intent**, not its name: no title is collected
   (#1211 S2) and the sentence is not delivered from here yet (#1299) — which is
   exactly why the landing below has to open the spec composer. */
async function composerOnScreen() {
  await userEvent.type(await screen.findByLabelText('What this wave should do'), 'Read it');
}

async function createAWave() {
  /* Exact: the rail's per-area opener is `New wave in Work`, which a substring
     match would also find — the area page's own `+` is the one this drives. */
  await userEvent.click(await screen.findByRole('button', { name: /^New wave$/ }));
  await composerOnScreen();
  await userEvent.click(await screen.findByRole('button', { name: 'Create wave' }));
}

/** The same create, started from the rail — which is reachable from *every*
 *  route, including a wave page. */
async function createAWaveFromTheRail() {
  await userEvent.click(await screen.findByRole('button', { name: 'New wave in Work' }));
  await composerOnScreen();
  await userEvent.click(await screen.findByRole('button', { name: 'Create wave' }));
}

async function goToWave(router: ReturnType<typeof setup>['router'], waveId: string) {
  await act(async () => { await router.navigate({ to: '/wave/$waveId', params: { waveId } }); });
}

beforeEach(() => {
  window.history.pushState({}, '', `${APP_BASEPATH}/area/c1`);
  /* The drawer, the composer and `EditableTitle` all move focus inside a frame.
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
   * The landing, end to end. Nothing is typed into the composer — the sentence
   * is not delivered from it yet (#1299) — and what the reader gets is the spec
   * conversation open with the caret in the composer, because their first
   * sentence is the wave's intent.
   *
   * Red when: the create route stops stating the intent, `WaveRouteBody` stops
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
   * The rail's per-area `+` is rendered by `AppShell`, above the route outlet,
   * so it is on screen on every route — "read one wave, start another" is an
   * ordinary move, and no layer covered it before this slice.
   *
   * **What this case is and is not.** It was written to reproduce a specific
   * defect in the replaced shape — a global intent slot that the *departing*
   * wave's route body could clear before the new route mounted — and it did
   * not reproduce it: both review channels and this run agree the old code was
   * green here, because the provider update and the navigation sat in one
   * Promise continuation and React 19 batches them, so no commit ever rendered
   * the old wave with the request already stated. It is therefore a
   * characterization test — it pins the outcome of a path that had none, on
   * the shape that is shipping — and not evidence that a bug was fixed.
   *
   * Red when the create started from the rail stops landing the reader in the
   * new wave's spec conversation, whatever the reason.
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
   * ── A fresh visit to the wave carries no intent ──────────────────────────
   *
   * The detail read fails, so `WaveRoute` returns the error box and the body
   * that would redeem the intent never mounts. The reader gives up, walks back
   * to the area, and later opens the wave again — a *new* navigation, so a new
   * history entry, and the mark is not on it. An ordinary visit does not spring
   * the drawer open and take the caret.
   *
   * This is the half of the per-entry semantics that says the mark does not
   * float free; the case below it is the other half, where the reader returns
   * to the very entry that failed and the mark is still there on purpose.
   *
   * Red when the intent is held anywhere that outlives its own history entry.
   */
  it('does not open on a later visit when the landing never reached the wave', async () => {
    const { router, gate } = setup({
      created: OTHER_WAVE, createdCards: [OTHER_SPEC_CARD], createdDetailFails: true,
    });
    await createAWave();
    await screen.findByRole('button', { name: 'Retry' });

    await act(async () => { await router.navigate({ to: '/area/$areaId', params: { areaId: 'c1' } }); });
    await screen.findByRole('button', { name: 'Rename area' });

    gate.createdDetailFails = false;
    await goToWave(router, 'w2');
    await screen.findByRole('button', { name: 'Rename wave' });
    expect(screen.queryByRole('complementary', { name: 'Spec chat' })).toBeNull();
  });

  /*
   * ── Returning to the entry that failed arms it again, and that is the deal ─
   *
   * Same setup as above, except the reader comes back with Back rather than by
   * navigating afresh — so this is the *same* history entry, the one the create
   * made and marked, and its mark was never redeemed because the body that
   * redeems it never mounted. It is still there, and this time the detail lands
   * and the conversation opens.
   *
   * That is the chosen semantics, not a leak: the mark belongs to the entry,
   * and the reachable readings of "display that entry again" are a reload and
   * the Retry button — both of them "the landing finally worked", both of them
   * wanting exactly this. Back is the same act on the same entry and cannot be
   * told apart from them without giving the intent a second, time-based owner,
   * which is the shape that had no owner at all. Pinned here so a later change
   * that makes the mark expire has to argue with a test rather than with a
   * comment.
   *
   * Red when the mark stops belonging to the entry — cleared on the way out,
   * or expired by anything other than being redeemed.
   */
  it('opens the conversation when Back returns to the entry whose landing had failed', async () => {
    const { router, gate } = setup({
      created: OTHER_WAVE, createdCards: [OTHER_SPEC_CARD], createdDetailFails: true,
    });
    await createAWave();
    await screen.findByRole('button', { name: 'Retry' });

    /* A push, so the failed entry stays underneath rather than being replaced. */
    await act(async () => { await router.navigate({ to: '/area/$areaId', params: { areaId: 'c1' } }); });
    await screen.findByRole('button', { name: 'Rename area' });

    gate.createdDetailFails = false;
    await act(async () => {
      router.history.back();
      await new Promise((resolve) => { setTimeout(resolve, 0); });
    });

    await waitFor(() => { expect(router.state.location.pathname.endsWith('/wave/w2')).toBe(true); });
    await screen.findByRole('complementary', { name: 'Spec chat' });
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
    await act(async () => { await router.navigate({ to: '/area/$areaId', params: { areaId: 'c1' } }); });
    await screen.findByRole('button', { name: 'Rename area' });
    expect(screen.queryByRole('complementary', { name: 'Spec chat' })).toBeNull();

    await act(async () => { await router.navigate({ to: '/wave/$waveId', params: { waveId: 'w1' } }); });
    await screen.findByRole('button', { name: 'Rename wave' });
    expect(screen.queryByRole('complementary', { name: 'Spec chat' })).toBeNull();
  });

  /*
   * A landing that found nothing to open must not leave the mark standing.
   *
   * "Nothing opened" on its own is not that claim — it is true of a page with
   * no spec card whatever the mark does, which is why this case goes on: the
   * spec card arrives on a *later* read of the same wave, on the same history
   * entry, and the drawer must still be shut. A reader three actions past the
   * create is not asking for a conversation.
   *
   * Red when the redemption stops disarming on the no-card arm.
   */
  it('opens nothing, and arms nothing, when the wave has no spec card', async () => {
    const { client, setCardsOf } = setup({ cards: [] });
    await createAWave();
    await screen.findByRole('button', { name: 'Rename wave' });
    expect(screen.queryByRole('complementary', { name: 'Spec chat' })).toBeNull();

    setCardsOf('w1', [SPEC_CARD]);
    await act(async () => { await client.invalidateQueries(); });
    /* The row for the spec card, which is proof the second read landed — and
       the only place `Spec chat` may appear, because the drawer is shut. */
    await screen.findByText('Spec chat');
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
   * and still does for an area — would leave "I cleared it, pressed Enter, and
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
