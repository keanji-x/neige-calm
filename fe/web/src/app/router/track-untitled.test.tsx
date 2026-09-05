// @vitest-environment jsdom
//
// #1211 S2 — a track that starts with no name and no words in it, driven
// through the real router, the real composer route and the real transport port.
//
// Two things had to be built for that to be a usable product rather than a
// blank page, and both are wiring that no single component can be asked about:
//
//   1. **Creating lands in the planner conversation, with the caret in it.** The
//      create site cannot name the card to open — `POST /api/tracks` answers
//      with a `Track` — so it marks the *navigation* it makes, and
//      `TrackRouteBody` redeems that mark against its own cards. That hand-off
//      spans three modules, which is exactly why it is asserted here and not in
//      any of them. Since #1211 S3 the create site is `/area/{id}/new` rather
//      than a dialog; the hand-off is the same one and this file drives it
//      through the page.
//   2. **Clearing the title is a request, not a cancel.** The track header
//      passes `emptyCommit="clear"` so the planner agent's `calm.track.rename` can
//      name the track again; what proves it is the PATCH on the wire.

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
/* The track the create answers with. `title: ''` is what the kernel stores when
   the POST omits the key, which is the whole point of the slice. */
const TRACK = {
  id: 'w1', area_id: 'c1', title: '', sort: 1, lifecycle: 'draft', cwd: '/tmp',
  archived_at: null, pinned_at: null, terminal_at: null, created_at: 1, updated_at: 2,
};
/* A named track, for the rename half: clearing a name that is already empty is
   the arithmetic no-op, so the PATCH case needs something to clear. */
const NAMED_TRACK = { ...TRACK, title: 'Test track' };
/* The track a *second* create answers with. It exists so the intent can be
   stated while the reader is standing on some other track — the rail's per-area
   `+` is rendered by `AppShell`, above the route outlet, so "read one track and
   start another" is an ordinary move and the track being left is still
   mounted. */
const OTHER_TRACK = { ...TRACK, id: 'w2', sort: 2, created_at: 3, updated_at: 3 };
const PLANNER_CARD = {
  id: 'card-planner', track_id: 'w1', kind: 'codex', title: 'Planner chat', sort: 1,
  payload: { planner_harness: true }, deletable: true, created_at: 1, updated_at: 2,
};
const OTHER_PLANNER_CARD = { ...PLANNER_CARD, id: 'card-planner-w2', track_id: 'w2' };

function ok(body: unknown): ApiTransportResponse {
  return { status: 200, statusText: 'OK', body };
}

type Options = {
  track?: typeof TRACK;
  cards?: readonly unknown[];
  /** What `POST /api/tracks` answers with. Defaults to the track already listed,
   *  which is the single-track shape most of these cases want. */
  created?: typeof TRACK;
  createdCards?: readonly unknown[];
  /** Start with the created track's detail failing, so the reader lands on the
   *  error box and the route body never mounts. Flipped back through the
   *  returned `gate`. */
  createdDetailFails?: boolean;
};

function setup(options: Options = {}) {
  const track = options.track ?? TRACK;
  const cards = options.cards ?? [PLANNER_CARD];
  const created = options.created ?? track;
  const createdCards = options.createdCards ?? cards;
  const gate = { createdDetailFails: options.createdDetailFails ?? false };
  const details = new Map<string, { track: typeof TRACK; cards: readonly unknown[] }>([
    [track.id, { track, cards }],
    [created.id, { track: created, cards: createdCards }],
  ]);
  const requests: ApiRequest[] = [];
  const values = new Map<string, string>();
  const transport: ApiTransportPort = {
    send(request) {
      requests.push(request);
      if (request.path === '/api/areas') return Promise.resolve(ok([AREA]));
      if (request.path === '/api/areas/c1/tracks') {
        return Promise.resolve(ok(created.id === track.id ? [track] : [track, created]));
      }
      if (request.path === '/api/overlays?entity_kind=track') return Promise.resolve(ok([]));
      if (request.path === '/api/track-templates') return Promise.resolve(ok([]));
      if (request.method === 'POST' && request.path === '/api/tracks') return Promise.resolve(ok(created));
      const patched = request.method === 'PATCH' ? details.get(request.path.slice('/api/tracks/'.length)) : undefined;
      if (patched !== undefined) {
        return Promise.resolve(ok({ ...patched.track, ...(request.body as object) }));
      }
      if (request.path.endsWith('/conversations')) return Promise.resolve(ok([]));
      const detail = details.get(request.path.slice('/api/tracks/'.length));
      if (detail !== undefined) {
        if (detail.track.id === created.id && created.id !== track.id && gate.createdDetailFails) {
          return Promise.resolve({ status: 500, statusText: 'Server Error', body: {} });
        }
        return Promise.resolve(ok({
          track: detail.track, can_resume: false, cards: detail.cards, overlays: [],
        }));
      }
      if (request.path.endsWith('/planner/run')) {
        return Promise.resolve(ok({ card_id: PLANNER_CARD.id, worker_session_id: 'r', phase: 'idle' }));
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
    /** Change what the next read of a track's detail answers with. */
    setCardsOf: (trackId: string, next: readonly unknown[]) => {
      const detail = details.get(trackId);
      if (detail !== undefined) details.set(trackId, { track: detail.track, cards: next });
    },
    client,
  };
}

/* The composer's field. `combobox` and not `textbox`: the track route passes
   `onNewConversation`, which arms the `/` trigger menu and turns Astryx's
   editable into a combobox. */
function messageField(): HTMLElement {
  return screen.getByRole('combobox', { name: 'Message' });
}

/* The composer page, with the one thing it does ask for typed into it.
   Since #1211 S3 the `+` navigates to `/area/{id}/new` instead of opening a
   dialog, so a create waits for the page's own field rather than
   `role="dialog"`, and Create stays disabled until that field says something.
   What is typed is the track's **intent**, not its name: no title is collected
   (#1211 S2) and the sentence goes out on the create as `first_message` (#1299)
   — which is why the landing below has to open the planner conversation, where
   it has just been delivered. */
async function composerOnScreen() {
  await userEvent.type(await screen.findByLabelText('What this track should do'), 'Read it');
}

async function createATrack() {
  await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
  await composerOnScreen();
  await userEvent.click(await screen.findByRole('button', { name: 'Create track' }));
}

/** The same create, started from the rail — which is reachable from *every*
 *  route, including a track page. */
async function createATrackFromTheRail() {
  await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
  await composerOnScreen();
  await userEvent.click(await screen.findByRole('button', { name: 'Create track' }));
}

async function goToTrack(router: ReturnType<typeof setup>['router'], trackId: string) {
  await act(async () => { await router.navigate({ to: '/track/$trackId', params: { trackId } }); });
}

beforeEach(() => {
  window.history.pushState({}, '', `${APP_BASEPATH}/`);
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

describe('creating a track lands in its planner conversation', () => {
  /*
   * The landing, end to end. The sentence typed on the create page went with
   * the create (#1299), and what the reader gets is the planner conversation
   * open with the caret in the composer — the thread their intent was just
   * delivered into, ready for whatever they say next.
   *
   * Red when: the create route stops stating the intent, `TrackRouteBody` stops
   * redeeming it, the panel drops the `focusComposer` flag on the way to the
   * composer, or `ChatComposer` stops honouring `focusOnMount`.
   */
  it('opens the planner conversation with the caret in the composer', async () => {
    setup();
    await createATrack();
    await screen.findByRole('complementary', { name: 'Planner chat' });
    await waitFor(() => { expect(document.activeElement).toBe(messageField()); });
  });

  /*
   * ── The same landing, started from a track page ───────────────────────────
   *
   * The rail's per-area `+` is rendered by `AppShell`, above the route outlet,
   * so it is on screen on every route — "read one track, start another" is an
   * ordinary move, and no layer covered it before this slice.
   *
   * **What this case is and is not.** It was written to reproduce a specific
   * defect in the replaced shape — a global intent slot that the *departing*
   * track's route body could clear before the new route mounted — and it did
   * not reproduce it: both review channels and this run agree the old code was
   * green here, because the provider update and the navigation sat in one
   * Promise continuation and React 19 batches them, so no commit ever rendered
   * the old track with the request already stated. It is therefore a
   * characterization test — it pins the outcome of a path that had none, on
   * the shape that is shipping — and not evidence that a bug was fixed.
   *
   * Red when the create started from the rail stops landing the reader in the
   * new track's planner conversation, whatever the reason.
   */
  it('opens the planner conversation of the new track when the create started on another track', async () => {
    const { router } = setup({ created: OTHER_TRACK, createdCards: [OTHER_PLANNER_CARD] });
    await goToTrack(router, 'w1');
    await screen.findByRole('button', { name: 'Rename track' });

    await createATrackFromTheRail();

    await waitFor(() => { expect(router.state.location.pathname.endsWith('/track/w2')).toBe(true); });
    /* The drawer lives in `TrackRouteBody`, which is keyed by track and unmounts
       with the route — so a drawer open on this page is this track's own. */
    await screen.findByRole('complementary', { name: 'Planner chat' });
    await waitFor(() => { expect(document.activeElement).toBe(messageField()); });
  });

  /*
   * ── A fresh visit to the track carries no intent ──────────────────────────
   *
   * The detail read fails, so `TrackRoute` returns the error box and the body
   * that would redeem the intent never mounts. The reader gives up, returns to
   * Today, and later opens the track again — a *new* navigation, so a new
   * history entry, and the mark is not on it. An ordinary visit does not spring
   * the drawer open and take the caret.
   *
   * This is the half of the per-entry semantics that says the mark does not
   * float free; the case below it is the other half, where the reader returns
   * to the very entry that failed and the mark is still there on purpose.
   *
   * Red when the intent is held anywhere that outlives its own history entry.
   */
  it('does not open on a later visit when the landing never reached the track', async () => {
    const { router, gate } = setup({
      created: OTHER_TRACK, createdCards: [OTHER_PLANNER_CARD], createdDetailFails: true,
    });
    await createATrack();
    await screen.findByRole('button', { name: 'Retry' });

    await act(async () => { await router.navigate({ to: '/' }); });
    expect(router.state.location.pathname).toBe('/');

    gate.createdDetailFails = false;
    await goToTrack(router, 'w2');
    await screen.findByRole('button', { name: 'Rename track' });
    expect(screen.queryByRole('complementary', { name: 'Planner chat' })).toBeNull();
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
      created: OTHER_TRACK, createdCards: [OTHER_PLANNER_CARD], createdDetailFails: true,
    });
    await createATrack();
    await screen.findByRole('button', { name: 'Retry' });

    /* A push, so the failed entry stays underneath rather than being replaced. */
    await act(async () => { await router.navigate({ to: '/' }); });
    expect(router.state.location.pathname).toBe('/');

    gate.createdDetailFails = false;
    await act(async () => {
      router.history.back();
      await new Promise((resolve) => { setTimeout(resolve, 0); });
    });

    await waitFor(() => { expect(router.state.location.pathname.endsWith('/track/w2')).toBe(true); });
    await screen.findByRole('complementary', { name: 'Planner chat' });
  });

  /*
   * And it is a one-shot. The intent is cleared as it is redeemed, so walking
   * back into the same track later is an ordinary visit — a reader who closed
   * the conversation must not have it forced open again every time.
   *
   * Red when the mark is not consumed — left on the entry, or read from
   * somewhere that outlives the navigation that wrote it.
   */
  it('does not re-open the conversation on a later visit to the same track', async () => {
    const { router } = setup();
    await createATrack();
    await screen.findByRole('complementary', { name: 'Planner chat' });

    /* Leaving the route takes the drawer with it — that is the "closed" state
       this case needs, and it is one the reader reaches every time they walk
       away. Coming back is then an ordinary visit, and it must stay one. */
    await act(async () => { await router.navigate({ to: '/' }); });
    expect(router.state.location.pathname).toBe('/');
    expect(screen.queryByRole('complementary', { name: 'Planner chat' })).toBeNull();

    await act(async () => { await router.navigate({ to: '/track/$trackId', params: { trackId: 'w1' } }); });
    await screen.findByRole('button', { name: 'Rename track' });
    expect(screen.queryByRole('complementary', { name: 'Planner chat' })).toBeNull();
  });

  /*
   * A landing that found nothing to open must not leave the mark standing.
   *
   * "Nothing opened" on its own is not that claim — it is true of a page with
   * no planner card whatever the mark does, which is why this case goes on: the
   * planner card arrives on a *later* read of the same track, on the same history
   * entry, and the drawer must still be shut. A reader three actions past the
   * create is not asking for a conversation.
   *
   * Red when the redemption stops disarming on the no-card arm.
   */
  it('opens nothing, and arms nothing, when the track has no planner card', async () => {
    const { client, setCardsOf } = setup({ cards: [] });
    await createATrack();
    await screen.findByRole('button', { name: 'Rename track' });
    expect(screen.queryByRole('complementary', { name: 'Planner chat' })).toBeNull();

    setCardsOf('w1', [PLANNER_CARD]);
    await act(async () => { await client.invalidateQueries(); });
    /* The row for the planner card, which is proof the second read landed — and
       the only place `Planner chat` may appear, because the drawer is shut. */
    await screen.findByText('Planner chat');
    expect(screen.queryByRole('complementary', { name: 'Planner chat' })).toBeNull();
  });
});

describe('clearing the track title', () => {
  /*
   * `emptyCommit="clear"` on the track header, proved on the wire.
   *
   * The empty title is the one state `calm.track.rename` will fill in (#1211
   * S3), so "clear the name" is how a reader hands naming back to the agent.
   * Swallowing the keystroke — which is what the primitive does by default,
   * and still does for an area — would leave "I cleared it, pressed Enter, and
   * nothing happened".
   *
   * Red when the track page drops `emptyCommit`, or the primitive stops
   * honouring it.
   */
  it('PATCHes an empty title when the box is emptied and committed', async () => {
    const { requests, router } = setup({ track: NAMED_TRACK });
    await act(async () => { await router.navigate({ to: '/track/$trackId', params: { trackId: 'w1' } }); });
    await userEvent.click(await screen.findByRole('button', { name: 'Rename track' }));
    await userEvent.clear(screen.getByRole('textbox', { name: 'Track title' }));
    await userEvent.type(screen.getByRole('textbox', { name: 'Track title' }), '{Enter}');

    await waitFor(() => {
      expect(requests.filter((request) => request.method === 'PATCH' && request.path === '/api/tracks/w1'))
        .toEqual([expect.objectContaining({ body: { title: '' } })]);
    });
  });
});
