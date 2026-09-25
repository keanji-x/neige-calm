// @vitest-environment jsdom
// The new-track page: `/area/{id}/new`, reached from each Area group's `+`.
import { onlineManager, QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { RouterProvider } from '@tanstack/react-router';
import { StrictMode } from 'react';
import { act, cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';
import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { APP_BASEPATH, createAppRouter } from './public.tsx';
import { bootTestCardRuntime } from './test-card-runtime.ts';
import { ThemeProvider } from '../theme/public.tsx';

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

afterEach(() => { cleanup(); onlineManager.setOnline(true); delete document.documentElement.dataset.theme; });

function memoryStorage() {
  const values = new Map<string, string>();
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
  };
}

/* astryx puts `label` on the `contenteditable` as `aria-label`, so the composer resolves by label query. */
const TASK_LABEL = 'What this track should do';

const FOLDER_PLACEHOLDER = 'Neige workspace';
const FOLDER_CHIP_NAME = `Folder: ${FOLDER_PLACEHOLDER}`;

/* The template chip always names the current choice, "No template" until one is picked. */
const TEMPLATE_CHIP = /^Template: /;

const AREA = { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1 };
const OTHER = { id: 'c2', name: 'Reading', color: '#8B7FE8', sort: 2, kind: 'user', created_at: 1, updated_at: 1 };

const LISTING = {
  path: '/srv/app', parent: '/srv', entries: [{ name: 'crates', is_dir: true }],
};

/* The kernel returns an empty title; the planner agent names the track later through `calm.track.rename`. */
const TRACK_ROW = {
  id: 'w-new', area_id: 'c1', title: '', sort: 0, archived_at: null, pinned_at: null,
  lifecycle: 'draft', cwd: '/srv/managed', template_id: null, plugin_scope: null,
  purpose: null, template_input: null, terminal_at: null, created_at: 1, updated_at: 1,
};

/** The 409 `POST /api/tracks` answers a folder clash with — no `error` key. */
const CONFLICT = {
  folder_id: 4, area_id: 'c1', conflict_path: '/srv/app', conflict_kind: 'descendant',
};

/* One template bound to a running plugin (an `input_schema`, therefore fields) and one that is not. */
const TEMPLATES = [
  { id: 'small-change', title: 'Small change', tasks: [{ key: 'inspect', goal: 'Read the change.' }] },
  {
    id: 'issue-development',
    title: 'Issue development',
    input_schema: { type: 'object', required: ['issue_url', 'repo', 'issue_number'] },
    tasks: [{ key: 'inspect-issue', goal: 'Read the bound issue.' }],
  },
];

function harness(options: {
  templates?: unknown;
  areaDefaults?: Readonly<{ default_template_id: string | null; default_cwd: string | null }>;
  otherAreaDefaults?: Readonly<{ default_template_id: string | null; default_cwd: string | null }>;
  trackCreate?: ApiTransportResponse;
  /** Ordered create outcomes for retry/recovery paths that cross Area scope. */
  trackCreateSequence?: readonly ApiTransportResponse[];
  loseFirstCreateAck?: boolean;
  /** Override the detail read the track page makes when the create lands. */
  trackDetail?: ApiTransportResponse;
  /** Hold the detail read open until this resolves, to drive a slow landing. */
  heldDetail?: Promise<void>;
  /** Hold the create POST open until this resolves, to drive a late create. */
  heldCreate?: Promise<void>;
  /** Hold `GET /api/areas` open, to render the page while the area's existence is genuinely unknown. */
  heldAreas?: Promise<void>;
  /** Fail `GET /api/areas` outright: a 500 leaves `workspace.areas` at `[]` with `areasLoading` false, indistinguishable from "landed, and this area is gone". */
  areasFail?: boolean;
  /** Where the browser starts, under the basepath; deep-linking is the only entry that reaches a stale area id. */
  path?: string;
  /** Rows the planner card's item read answers with; defaults to `[]`, what the kernel answers before the queue drains. */
  plannerItems?: readonly unknown[];
} = {}) {
  const sent: ApiRequest[] = [];
  let trackCreateIndex = 0;
  let firstCreateAckLost = false;
  const transport: ApiTransportPort = {
    send(request: ApiRequest): Promise<ApiTransportResponse> {
      sent.push(request);
      const posted = request.body as { area_id?: string } | undefined;
      if (request.method === 'POST' && request.path === '/api/tracks' && options.loseFirstCreateAck && !firstCreateAckLost) {
        firstCreateAckLost = true;
        return Promise.reject(new Error('Connection closed after server commit'));
      }
      if (request.method === 'POST' && request.path === '/api/tracks' && options.trackCreate) {
        return Promise.resolve(options.trackCreate);
      }
      if (request.method === 'POST' && request.path === '/api/tracks' && options.trackCreateSequence) {
        const response = options.trackCreateSequence[trackCreateIndex];
        trackCreateIndex += 1;
        if (response !== undefined) return Promise.resolve(response);
      }
      if (request.method === 'POST' && request.path === '/api/tracks' && options.heldCreate) {
        return options.heldCreate.then(() => ({
          status: 200,
          statusText: 'OK',
          body: { ...TRACK_ROW, area_id: posted?.area_id ?? 'c1' },
        }));
      }
      /* What `routes/models.rs` answers for a Claude Planner: no catalog and no default (#1791 §5.8). */
      if (request.path === '/api/models?provider=claude') {
        return Promise.resolve({ status: 200, statusText: 'OK', body: {
          models: [], default: { model: null, reasoning_effort: null }, default_source: 'unknown',
          source: 'unavailable', fetched_at_ms: null,
        } });
      }
      if (request.path === '/api/models?provider=codex') {
        return Promise.resolve({ status: 200, statusText: 'OK', body: {
          models: [{ id: 'fast', model: 'gpt-5', display_name: 'GPT-5', description: 'Everyday model',
            is_default: true, default_reasoning_effort: 'low', supported_reasoning_efforts: [
              { reasoning_effort: 'low', description: 'Answers sooner' },
              { reasoning_effort: 'high', description: 'Thinks longer' },
            ] }],
          default: { model: null, reasoning_effort: null }, default_source: 'unknown',
          source: 'live', fetched_at_ms: 1,
        } });
      }
      if (request.path === '/api/track-templates') {
        // `undefined` here is the read failing outright.
        const templates = options.templates;
        return templates === undefined
          ? Promise.resolve({ status: 500, statusText: 'Server Error', body: { message: 'boom' } })
          : Promise.resolve({ status: 200, statusText: 'OK', body: templates });
      }
      /* Served rather than left to fall through to `[]`: a decode failure would look
               identical to "the feature did not run". */
      if (request.method === 'GET' && request.path === '/api/tracks/w-new') {
        if (options.trackDetail) return Promise.resolve(options.trackDetail);
        const detail = {
          status: 200,
          statusText: 'OK',
          body: {
            track: { ...TRACK_ROW },
            can_resume: false,
            cards: [{
              id: 'card-planner', track_id: 'w-new', kind: 'codex', title: 'Planner',
              payload: { planner_harness: true }, sort: 0, created_at: 1, updated_at: 1,
            }],
            overlays: [],
          },
        } satisfies ApiTransportResponse;
        /* Held open on request, so a test can land the cards after the navigation. */
        return options.heldDetail
          ? options.heldDetail.then(() => detail)
          : Promise.resolve(detail);
      }
      if (request.method === 'GET' && request.path.includes('/harness/items')) {
        return Promise.resolve({ status: 200, statusText: 'OK', body: [...(options.plannerItems ?? [])] });
      }
      if (request.path === '/api/areas' && options.areasFail) {
        return Promise.resolve({ status: 500, statusText: 'Server Error', body: { error: 'areas are unreadable' } });
      }
      if (request.path === '/api/areas' && options.heldAreas) {
        return options.heldAreas.then(() => ({ status: 200, statusText: 'OK', body: [AREA, OTHER] }));
      }
      const body = request.path === '/api/areas' ? [
        { ...AREA, ...options.areaDefaults }, { ...OTHER, ...options.otherAreaDefaults },
      ]
        : request.path.startsWith('/api/fs/listdir') ? LISTING
          : request.method === 'POST' && request.path === '/api/tracks'
            ? { ...TRACK_ROW, area_id: posted?.area_id ?? 'c1' }
            : [];
      return Promise.resolve({ status: 200, statusText: 'OK', body });
    },
  };
  window.history.pushState({}, '', `${APP_BASEPATH}${options.path ?? '/'}`);
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } });
  const router = createAppRouter({
    transport, unauthorized, client, cards: bootTestCardRuntime(), onSignOut: vi.fn(),
  });
  /* `StrictMode`, because production runs it: its double-invoked effects are what
       catch a `useRef` latch written only in the cleanup arm. */
  render(
    <StrictMode>
      <QueryClientProvider client={client}>
        <ThemeProvider storage={memoryStorage()}>
          <RouterProvider router={router} />
        </ThemeProvider>
      </QueryClientProvider>
    </StrictMode>,
  );
  return { sent, client, router };
}

describe('New track model selection', () => {
  it('keeps model and effort in the Area draft and retries the original creation configuration', async () => {
    const { sent } = harness({ templates: [], loseFirstCreateAck: true });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    await findComposer();
    await userEvent.click(await screen.findByRole('button', { name: 'Model: Default' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: 'GPT-5' }));
    await userEvent.click(screen.getByRole('button', { name: 'Reasoning effort: low (the default)' }));
    await userEvent.click(screen.getByRole('menuitem', { name: /high/ }));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Use these settings from the first turn');
    await userEvent.click(screen.getByRole('button', { name: 'Go to Today' }));
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    expect(await screen.findByRole('button', { name: 'Model: Default' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Go to Today' }));
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    expect(await screen.findByRole('button', { name: 'Model: GPT-5' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Reasoning effort: high' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await screen.findByText('Transport request failed');
    const original = createdTrackRequests(sent)[0];
    expect(original?.body).toMatchObject({ model: 'gpt-5', reasoning_effort: 'high',
      first_message: 'Use these settings from the first turn' });
    expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Model: GPT-5' }).disabled).toBe(true);
    expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Reasoning effort: high' }).disabled).toBe(true);
    expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Planner: Codex' }).disabled).toBe(true);
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(2));
    expect(createdTrackRequests(sent)[1]?.body).toEqual(original?.body);
    expect(createdTrackRequests(sent)[1]?.headers).toEqual(original?.headers);
  });

  it('omits overrides after returning to the installation default', async () => {
    const { sent } = harness({ templates: [] });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    await findComposer();
    await userEvent.click(await screen.findByRole('button', { name: 'Model: Default' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: 'GPT-5' }));
    await userEvent.click(screen.getByRole('button', { name: 'Reasoning effort: low (the default)' }));
    await userEvent.click(screen.getByRole('menuitem', { name: /high/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Model: GPT-5' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'Default' }));
    expect(screen.queryByRole('button', { name: /^Reasoning effort:/ })).toBeNull();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Follow the default');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(1));
    expect(createdTrackRequests(sent)[0]?.body).not.toHaveProperty('model');
    expect(createdTrackRequests(sent)[0]?.body).not.toHaveProperty('reasoning_effort');
  });

  it('switching provider clears a retained model and effort', async () => {
    const { sent } = harness({ templates: [] });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    await findComposer();
    await userEvent.click(await screen.findByRole('button', { name: 'Model: Default' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: 'GPT-5' }));
    await userEvent.click(screen.getByRole('button', { name: 'Reasoning effort: low (the default)' }));
    await userEvent.click(screen.getByRole('menuitem', { name: /high/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Planner: Codex' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Claude/ }));
    /* The server's `unavailable` answer for Claude disables the picker; nothing retained shows through it. */
    await waitFor(() => expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Model: Default' }).disabled).toBe(true));
    expect(screen.queryByRole('button', { name: /^Reasoning effort:/ })).toBeNull();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Plan with Claude');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(1));
    const body = createdTrackRequests(sent)[0]?.body;
    expect(body).toMatchObject({ planner_provider: 'claude', first_message: 'Plan with Claude' });
    expect(body).not.toHaveProperty('model');
    expect(body).not.toHaveProperty('reasoning_effort');
  });

  it('switching back to Codex starts again from the installation default', async () => {
    harness({ templates: [] });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    await findComposer();
    await userEvent.click(await screen.findByRole('button', { name: 'Model: Default' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: 'GPT-5' }));
    await userEvent.click(screen.getByRole('button', { name: 'Planner: Codex' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Claude/ }));
    /* Reopened by keyboard: astryx swallows a trigger click that lands right after its menu hid. */
    (await screen.findByRole('button', { name: 'Planner: Claude' })).focus();
    await userEvent.keyboard('{ArrowDown}');
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Codex/ }));
    await waitFor(() => expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Model: Default' }).disabled).toBe(false));
  });

  it('shows a refused Claude create readably and lets the reader create on Codex instead', async () => {
    const refusal = 'bad request: track create: `planner_provider` `claude` is unavailable: '
      + 'calm-server was started without --claude-planner-config';
    const { sent } = harness({ templates: [], trackCreateSequence: [
      { status: 400, statusText: 'Bad Request', body: { error: refusal, code: 'bad_request' } },
    ] });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    await findComposer();
    await userEvent.click(screen.getByRole('button', { name: 'Planner: Codex' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Claude/ }));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Try Claude first');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    expect((await screen.findByRole('alert')).textContent).toContain('--claude-planner-config');
    expect(composerText()).toBe('Try Claude first');
    await userEvent.click(screen.getByRole('button', { name: 'Planner: Claude' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Codex/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(2));
    expect(createdTrackRequests(sent).map((request) => (request.body as { planner_provider: string }).planner_provider))
      .toEqual(['claude', 'codex']);
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`));
  });

  it('keeps the choice per Area draft and resets it once the track is created', async () => {
    harness({ templates: [] });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    await findComposer();
    await userEvent.click(screen.getByRole('button', { name: 'Planner: Codex' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Claude/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Go to Today' }));
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    expect(await screen.findByRole('button', { name: 'Planner: Codex' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Go to Today' }));
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    expect(await screen.findByRole('button', { name: 'Planner: Claude' })).toBeTruthy();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Plan with Claude');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`));
    await userEvent.click(screen.getByRole('button', { name: 'Go to Today' }));
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    expect(await screen.findByRole('button', { name: 'Planner: Codex' })).toBeTruthy();
  });

  it('names the Codex Planner backend on every create', async () => {
    const { sent } = harness({ templates: [] });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Start the Planner');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(1));
    expect(createdTrackRequests(sent)[0]?.body).toMatchObject({
      planner_provider: 'codex', first_message: 'Start the Planner',
    });
  });
});

describe('Track creation drafts survive navigation', () => {
  it.each(['offline', 'rate-limit', 'folder-conflict'] as const)('keeps an earlier unconfirmed request after a later %s rejection', async (rejection) => {
    const { sent } = harness({ templates: [], loseFirstCreateAck: true,
      otherAreaDefaults: { default_template_id: null, default_cwd: '/srv/app' },
      trackCreateSequence: rejection === 'offline' ? undefined : [
        rejection === 'rate-limit'
          ? { status: 429, statusText: 'Too Many Requests', body: { error: 'rate limited' } }
          : { status: 409, statusText: 'Conflict', body: CONFLICT },
      ],
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Keep original intent');
    document.documentElement.dataset.theme = 'light';
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await screen.findByText('Transport request failed');
    const original = createdTrackRequests(sent)[0];
    if (rejection === 'offline') act(() => onlineManager.setOnline(false));
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(screen.getByRole('alert').textContent).not.toContain('Transport request failed'));
    expect(screen.getByLabelText(TASK_LABEL).getAttribute('contenteditable')).toBe('false');
    expect(screen.queryByRole('button', { name: 'Create in Work' })).toBeNull();
    act(() => onlineManager.setOnline(true));
    await userEvent.click(screen.getByRole('button', { name: 'Go to Today' }));
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    document.documentElement.dataset.theme = 'dark';
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(rejection === 'offline' ? 2 : 3));
    const retry = createdTrackRequests(sent).at(-1);
    expect(retry?.headers).toEqual(original?.headers);
    expect(retry?.body).toEqual(original?.body);
  });

  it('restores unsent text and options independently for each Area', async () => {
    harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Unsent intent');
    await userEvent.click(screen.getByRole('button', { name: 'Template: No template' }));
    await userEvent.click(screen.getByRole('menuitem', { name: /Issue development/ }));
    await userEvent.type(screen.getByLabelText('Issue URL'), 'unfinished-url');
    await userEvent.click(screen.getByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    expect(composerText()).toBe('');
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Other draft');
    await userEvent.click(screen.getByRole('button', { name: 'New track in Work' }));
    await findComposer();
    expect(composerText()).toBe('Unsent intent');
    expect(screen.getByLabelText<HTMLInputElement>('Issue URL').value).toBe('unfinished-url');
    await userEvent.click(screen.getByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    expect(composerText()).toBe('Other draft');
  }, 15_000);

  it('retries the exact lost-ack request and key after leaving and returning', async () => {
    const { sent } = harness({ templates: TEMPLATES, loseFirstCreateAck: true });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), '  Create once  ');
    document.documentElement.dataset.theme = 'light';
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await screen.findByText('Transport request failed');
    const first = createdTrackRequests(sent)[0];
    expect(first?.body).toMatchObject({ theme: { fg: [42, 47, 58] } });
    await userEvent.click(screen.getByRole('button', { name: 'Go to Today' }));
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    await findComposer();
    expect(composerText()).toBe('  Create once  ');
    document.documentElement.dataset.theme = 'dark';
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(2));
    const retry = createdTrackRequests(sent)[1];
    expect(retry?.headers).toEqual(first?.headers);
    expect(retry?.body).toEqual(first?.body);
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`));
  });

  it('retains an unsent draft when its parent is deleted and prevents an orphan create', async () => {
    const { sent, client } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Keep this after Area deletion');
    await act(async () => { client.setQueryData(['areas'], []); await new Promise((resolve) => setTimeout(resolve, 0)); });
    expect(screen.getByLabelText(TASK_LABEL).textContent).toBe('Keep this after Area deletion');
    expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Create track' }).disabled).toBe(true);
    await userEvent.keyboard('{Enter}');
    expect(createdTrackRequests(sent)).toEqual([]);
    expect(screen.getByRole('main').textContent).toContain('draft');
  });

  it('keeps a pending creation leased across route remount and offers its late acknowledgement', async () => {
    let release!: () => void;
    const heldCreate = new Promise<void>((resolve) => { release = resolve; });
    const { sent } = harness({ templates: TEMPLATES, heldCreate });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Create while away');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await userEvent.click(screen.getByRole('button', { name: 'Go to Today' }));
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    expect(await screen.findByRole('button', { name: 'Creating…' })).toBeTruthy();
    expect(createdTrackRequests(sent)).toHaveLength(1);
    release();
    await userEvent.click(await screen.findByRole('button', { name: 'Open track' }));
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`));
    expect(createdTrackRequests(sent)).toHaveLength(1);
  });
});

/* No click promises the next screen synchronously, so every role query waits on the field itself. */
async function findComposer(): Promise<HTMLElement> {
  return screen.findByLabelText(TASK_LABEL);
}

/** Waits for the track page to be mounted — it is the surface that would redeem
 *  a leftover open request, so the assertion has to happen after it exists. */
async function findTrackPage(): Promise<HTMLElement> {
  return screen.findByRole('main');
}

/** What the composer currently holds. It is a `contenteditable`, not an input,
 *  so the value is its text rather than a `value` property. */
function composerText(): string {
  return screen.getByLabelText(TASK_LABEL).textContent ?? '';
}

/** The text of every first message delivered to the planner card. */
function plannerInputTexts(sent: readonly ApiRequest[]): unknown[] {
  return sent.filter((request) => request.method === 'POST' && request.path.endsWith('/planner/input'))
    .map((request) => (request.body as { text?: unknown } | undefined)?.text);
}

function createdTrackBodies(sent: readonly ApiRequest[]): unknown[] {
  return createdTrackRequests(sent).map((request) => request.body);
}

function createdTrackRequests(sent: readonly ApiRequest[]): ApiRequest[] {
  return sent.filter((request) => request.method === 'POST' && request.path === '/api/tracks');
}

describe('the new-track page is a route reached from Area groups', () => {
  it('carries the selected group into one shared create route', async () => {
    harness();
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    expect(await findComposer()).toBeTruthy();
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c2/new`);

    expect(screen.queryByRole('dialog')).toBeNull();

    window.history.back();
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    expect(await findComposer()).toBeTruthy();
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c1/new`);
  });

  it('remounts the route when switching directly between Areas and takes only the new Area defaults', async () => {
    const { sent } = harness({
      templates: TEMPLATES,
      areaDefaults: { default_template_id: 'small-change', default_cwd: '/srv/work-a' },
      otherAreaDefaults: { default_template_id: null, default_cwd: '/srv/work-b' },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    await findComposer();
    expect(screen.getByRole('button', { name: 'Template: Small change' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Folder: /srv/work-a' })).toBeTruthy();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Draft for A');

    await userEvent.click(screen.getByRole('button', { name: 'New track in Reading' }));
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c2/new`));
    expect(composerText()).toBe('');
    expect(screen.getByRole('button', { name: 'Template: No template' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Folder: /srv/work-b' })).toBeTruthy();

    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Draft for B');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    const body = createdTrackBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({ area_id: 'c2', cwd: '/srv/work-b', attach_folder: true });
    expect(body).not.toHaveProperty('template_id');
  });

  it('does not let a create started in one Area navigate away after switching to another Area', async () => {
    let releaseCreate!: () => void;
    const heldCreate = new Promise<void>((resolve) => { releaseCreate = resolve; });
    const { sent } = harness({ templates: TEMPLATES, heldCreate });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Create in A');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));

    await userEvent.click(screen.getByRole('button', { name: 'New track in Reading' }));
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c2/new`));
    releaseCreate();
    await act(async () => {
      await heldCreate;
      await new Promise((resolve) => { setTimeout(resolve, 0); });
    });
    await new Promise((resolve) => { setTimeout(resolve, 0); });
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c2/new`);
  });

  it('arrives with the composer focused, so typing reaches it', async () => {
    harness();
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await act(async () => { await new Promise((resolve) => { requestAnimationFrame(() => resolve(null)); }); });

    expect(document.activeElement).toBe(screen.getByLabelText(TASK_LABEL));

    await userEvent.keyboard('Read it');
    expect(composerText()).toBe('Read it');
  });

  /* Asserted on key absence: `title: ''` reaches the same stored value, and only a
       missing key leaves `calm.track.rename` able to name the track. */
  it('creates with no title control on screen and no title key on the wire', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    // Exposed first, so the absence checks below cannot pass vacuously.
    expect(await findComposer()).toBeTruthy();
    /* By name, not by counting textboxes: the composer and the template input are textboxes too. */
    expect(screen.queryByRole('textbox', { name: /title/i })).toBeNull();
    expect(screen.queryByRole('combobox', { name: /title/i })).toBeNull();
    expect(screen.queryByLabelText(/title/i)).toBeNull();

    const create = await screen.findByRole('button', { name: 'Create track' });
    expect(create.hasAttribute('disabled')).toBe(true);
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    expect(create.hasAttribute('disabled')).toBe(false);
    await userEvent.click(create);
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    const body = createdTrackBodies(sent)[0] as Record<string, unknown>;
    expect(Object.hasOwn(body, 'title')).toBe(false);
    expect(body).toMatchObject({ area_id: 'c2' });
  });

  it('posts the opener\'s area_id and omits cwd / attach_folder with no folder chosen', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    // Exposed first: the absence checks below would pass vacuously against a page that never rendered.
    expect(await findComposer()).toBeTruthy();
    expect(screen.queryByLabelText('Area')).toBeNull();
    expect(screen.getByRole('button', { name: FOLDER_CHIP_NAME }).textContent).toBe(FOLDER_PLACEHOLDER);
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(await screen.findByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    const body = createdTrackBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({ area_id: 'c2' });
    expect(body).toHaveProperty('theme');
    expect(body).not.toHaveProperty('title');
    expect(body).toMatchObject({ first_message: 'Read it' });
    expect(plannerInputTexts(sent)).toEqual([]);
    // The managed-workspace branch is keyed on *absence*, not on a value:
    // `cwd: null` and `attach_folder: false` are both a different kernel path.
    expect(body).not.toHaveProperty('cwd');
    expect(body).not.toHaveProperty('attach_folder');
    expect(sent.some((request) => request.path.startsWith('/api/fs/listdir'))).toBe(false);
    // Blank means the key is not on the wire at all: `template_id: null` or `''` is a 400 from the kernel.
    expect(body).not.toHaveProperty('template_id');
    expect(body).not.toHaveProperty('template_input');
  });

  it('turns the Area defaults into an explicit template and attached-folder request', async () => {
    const message = '  Read it exactly  ';
    const { sent } = harness({
      templates: TEMPLATES,
      areaDefaults: { default_template_id: 'small-change', default_cwd: '/srv/ area ' },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    expect(await findComposer()).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Template: Small change' })).toBeTruthy();
    const folder = screen.getByRole('button', { name: 'Folder: /srv/ area' });
    expect(folder.getAttribute('aria-label')).toBe('Folder: /srv/ area ');
    await userEvent.type(screen.getByLabelText(TASK_LABEL), message);
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    expect(createdTrackBodies(sent)[0]).toMatchObject({
      area_id: 'c1',
      first_message: message,
      template_id: 'small-change',
      cwd: '/srv/ area ',
      attach_folder: true,
    });
  });

  it('lets one Track clear Area defaults back to a new managed folder', async () => {
    const { sent } = harness({
      templates: TEMPLATES,
      areaDefaults: { default_template_id: 'small-change', default_cwd: '/srv/area-default' },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    expect(await findComposer()).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: TEMPLATE_CHIP }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^No template/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Use a Neige workspace instead' }));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    const body = createdTrackBodies(sent)[0] as Record<string, unknown>;
    expect(body).not.toHaveProperty('template_id');
    expect(body).not.toHaveProperty('cwd');
    expect(body).not.toHaveProperty('attach_folder');
  });

  /* The kernel forwards `first_message` untrimmed and hashes it untrimmed, so surrounding whitespace is content. */
  it('posts the sentence exactly as typed, whitespace and all', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    const padded = '  keep indentation  ';
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), padded);
    await userEvent.click(await screen.findByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    expect(createdTrackBodies(sent)[0]).toMatchObject({ first_message: padded });
  });

  /* With `attach_folder` omitted the kernel refuses any path no area has already claimed. */
  it('posts the picked folder as cwd with attach_folder: true', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    expect(await findComposer()).toBeTruthy();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');

    await userEvent.click(await screen.findByRole('button', { name: FOLDER_CHIP_NAME }));
    expect(await screen.findByRole('dialog', { name: 'Choose a directory' })).toBeTruthy();
    await screen.findByDisplayValue('/srv/app/');
    await userEvent.click(await screen.findByRole('button', { name: 'Select this directory' }));
    expect(await findComposer()).toBeTruthy();

    await userEvent.click(await screen.findByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    const body = createdTrackBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({
      area_id: 'c1', cwd: '/srv/app', attach_folder: true,
    });
    expect(sent.some((request) => request.path === '/api/fs/listdir')).toBe(true);
    expect(body).not.toHaveProperty('template_id');
  });

  it('carries a chosen folder and a chosen template on the same POST', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Work' }));
    expect(await findComposer()).toBeTruthy();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: TEMPLATE_CHIP }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Small change/ }));

    await userEvent.click(await screen.findByRole('button', { name: FOLDER_CHIP_NAME }));
    await screen.findByDisplayValue('/srv/app/');
    await userEvent.click(await screen.findByRole('button', { name: 'Select this directory' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Create track' }));

    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    expect(createdTrackBodies(sent)[0]).toMatchObject({
      area_id: 'c1',
      template_id: 'small-change',
      cwd: '/srv/app',
      attach_folder: true,
    });
  });

  /* The 409 body has no `error` key, so `ApiError.message` is the bare status text. */
  it('renders the structured folder conflict, not the word Conflict', async () => {
    harness({
      templates: TEMPLATES,
      trackCreate: { status: 409, statusText: 'Conflict', body: CONFLICT },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    expect(await findComposer()).toBeTruthy();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(await screen.findByRole('button', { name: 'Create track' }));
    // The request, its rejection, and the re-render are three ticks the click
    // does not await; the default 1s window is not enough under a loaded suite.
    const alert = await screen.findByRole('alert', {}, { timeout: 5_000 });
    expect(alert.textContent).toContain('/srv/app');
    // `c1` is Work in the seeded area list — the id must never reach the page.
    expect(alert.textContent).toContain('area “Work”');
    expect(alert.textContent).not.toContain('c1');
    expect(alert.textContent).not.toBe('Conflict');
  });

  it.each(['equal', 'descendant'] as const)(
    'explicitly reuses the claimed directory in the current Area for a %s conflict',
    async (conflictKind) => {
    const { sent } = harness({
      templates: TEMPLATES,
      otherAreaDefaults: { default_template_id: null, default_cwd: '/srv/app' },
      trackCreateSequence: [
        { status: 409, statusText: 'Conflict', body: { ...CONFLICT, conflict_kind: conflictKind } },
        { status: 201, statusText: 'Created', body: { ...TRACK_ROW, area_id: 'c1' } },
      ],
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: TEMPLATE_CHIP }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Small change/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    const alert = await screen.findByRole('alert', {}, { timeout: 5_000 });
    await userEvent.click(within(alert).getByRole('button', { name: 'Reuse directory in Reading' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(2));
    const [failed, recovered] = createdTrackRequests(sent);
    expect(failed?.body).toMatchObject({
      area_id: 'c2', cwd: '/srv/app', attach_folder: true,
      first_message: 'Read it', template_id: 'small-change',
    });
    expect(recovered?.body).toMatchObject({
      area_id: 'c2', cwd: '/srv/app', attach_folder: true,
      first_message: 'Read it', template_id: 'small-change',
      allow_cross_area_cwd: { folder_id: 4, area_id: 'c1' },
    });
    expect(recovered?.headers?.['Idempotency-Key']).toBeDefined();
    expect(recovered?.headers?.['Idempotency-Key']).not.toBe(failed?.headers?.['Idempotency-Key']);
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`));
    },
  );

  it('unlocks the draft when vanished directory consent is rejected as invalid input', async () => {
    const { sent } = harness({
      templates: [],
      otherAreaDefaults: { default_template_id: null, default_cwd: '/srv/app' },
      trackCreateSequence: [
        { status: 409, statusText: 'Conflict', body: CONFLICT },
        { status: 400, statusText: 'Bad Request', body: {
          code: 'bad_request', error: 'The authorized folder claim no longer covers this cwd',
        } },
        { status: 201, statusText: 'Created', body: { ...TRACK_ROW, area_id: 'c2' } },
      ],
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Original intent');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Reuse directory in Reading' }));
    await screen.findByText('The authorized folder claim no longer covers this cwd');
    const message = screen.getByLabelText(TASK_LABEL);
    await userEvent.clear(message);
    await userEvent.type(message, 'Current intent');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(3));
    const request = createdTrackRequests(sent)[2];
    expect(request?.body).toMatchObject({ area_id: 'c2', cwd: '/srv/app', first_message: 'Current intent' });
    expect(request?.body).not.toHaveProperty('allow_cross_area_cwd');
  });

  it('does not offer an owning-Area retry for an ancestor conflict that moving cannot resolve', async () => {
    harness({
      templates: TEMPLATES,
      trackCreate: {
        status: 409,
        statusText: 'Conflict',
        body: { ...CONFLICT, conflict_kind: 'ancestor' },
      },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    const alert = await screen.findByRole('alert', {}, { timeout: 5_000 });
    expect(within(alert).queryByRole('button', { name: 'Reuse directory in Reading' })).toBeNull();
  });

  it('uses the visible current draft when the owning-Area recovery is clicked', async () => {
    const { sent } = harness({
      templates: TEMPLATES,
      otherAreaDefaults: { default_template_id: null, default_cwd: '/srv/app' },
      trackCreateSequence: [
        { status: 409, statusText: 'Conflict', body: CONFLICT },
        { status: 201, statusText: 'Created', body: { ...TRACK_ROW, area_id: 'c1' } },
      ],
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    const field = screen.getByLabelText(TASK_LABEL);
    await userEvent.type(field, 'Old message');
    await userEvent.click(screen.getByRole('button', { name: TEMPLATE_CHIP }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Small change/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    const alert = await screen.findByRole('alert', {}, { timeout: 5_000 });
    await userEvent.clear(field);
    await userEvent.type(field, 'Current message');
    await userEvent.click(screen.getByRole('button', { name: TEMPLATE_CHIP }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^No template/ }));
    await userEvent.click(within(alert).getByRole('button', { name: 'Reuse directory in Reading' }));

    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(2));
    const recovered = createdTrackRequests(sent)[1];
    expect(recovered?.body).toMatchObject({
      area_id: 'c2', first_message: 'Current message',
      allow_cross_area_cwd: { folder_id: 4, area_id: 'c1' },
    });
    expect(recovered?.body).not.toHaveProperty('template_id');
    expect(recovered?.body).toMatchObject({ cwd: '/srv/app', attach_folder: true });
  });

  it('withdraws the owning-Area recovery when the conflicting folder changes', async () => {
    const { sent } = harness({
      templates: TEMPLATES,
      otherAreaDefaults: { default_template_id: null, default_cwd: '/srv/app' },
      trackCreateSequence: [
        { status: 409, statusText: 'Conflict', body: CONFLICT },
        { status: 201, statusText: 'Created', body: { ...TRACK_ROW, area_id: 'c2' } },
      ],
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    const alert = await screen.findByRole('alert', {}, { timeout: 5_000 });
    expect(within(alert).getByRole('button', { name: 'Reuse directory in Reading' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Use a Neige workspace instead' }));
    expect(within(alert).queryByRole('button', { name: 'Reuse directory in Reading' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(2));
    const retried = createdTrackRequests(sent)[1];
    expect(retried?.body).toMatchObject({ area_id: 'c2', first_message: 'Read it' });
    expect(retried?.body).not.toHaveProperty('cwd');
    expect(retried?.body).not.toHaveProperty('attach_folder');
  });

  it('carries the chosen template onto the create POST', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Fix the thing');
    /* The option only exists once the template read has landed, so the wait is on the option, not the trigger. */
    await userEvent.click(screen.getByRole('button', { name: TEMPLATE_CHIP }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Issue development/ }));
    await userEvent.type(
      screen.getByLabelText('Issue URL'),
      'https://github.com/keanji-x/neige-calm/issues/1209',
    );
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    expect(createdTrackBodies(sent)[0]).toMatchObject({
      area_id: 'c2',
      template_id: 'issue-development',
      template_input: {
        issue_url: 'https://github.com/keanji-x/neige-calm/issues/1209',
        repo: 'keanji-x/neige-calm',
        issue_number: 1209,
        merge_policy: 'hold-for-ratify',
      },
    });
  });

  it('sends an unbound template as an id with no template_input', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Tiny fix');
    await userEvent.click(screen.getByRole('button', { name: TEMPLATE_CHIP }));
    await userEvent.click(await screen.findByRole('menuitem', { name: /^Small change/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    const body = createdTrackBodies(sent)[0] as Record<string, unknown>;
    expect(body).toMatchObject({ template_id: 'small-change', first_message: 'Tiny fix' });
    expect(body).not.toHaveProperty('title');
    expect(plannerInputTexts(sent)).toEqual([]);
    expect(body).not.toHaveProperty('template_input');
  });

  it('still creates a track when the template read fails outright', async () => {
    const { sent } = harness();
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    /* Wait for the failure to land, not for the request to leave: waiting on `sent`
           only proves the query started. */
    await screen.findByText(/Could not load templates/);
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it anyway');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    expect(createdTrackBodies(sent)[0]).toMatchObject({
      area_id: 'c2', first_message: 'Read it anyway',
    });
    expect(createdTrackBodies(sent)[0]).not.toHaveProperty('title');
    expect(plannerInputTexts(sent)).toEqual([]);
  });
});

describe('the route refuses an area id that no longer exists', () => {
  it('reports a deleted area instead of rendering a working composer', async () => {
    harness({ templates: TEMPLATES, path: '/area/c9/new' });
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain('This area could not be found.');
    expect(screen.queryByLabelText(TASK_LABEL)).toBeNull();
    expect(screen.queryByRole('button', { name: 'Create track' })).toBeNull();
  });

  /* The load-bearing assertion is the one before `releaseAreas()`: a route that
       renders the form while the read is in flight would still satisfy a bare
       `findComposer()` at the end. */
  it('renders the composer for an area that does exist, and only once the list has landed', async () => {
    let releaseAreas = (): void => undefined;
    const held = new Promise<void>((resolve) => { releaseAreas = () => { resolve(); }; });
    harness({ templates: TEMPLATES, path: '/area/c1/new', heldAreas: held });

    await screen.findByRole('button', { name: 'Go to Today' });
    expect(screen.queryByLabelText(TASK_LABEL)).toBeNull();
    expect(screen.queryByRole('button', { name: 'Create track' })).toBeNull();

    releaseAreas();
    await held;
    expect(await findComposer()).toBeTruthy();
    expect(screen.queryByRole('alert')).toBeNull();
  });

  /* `workspace.areas` is `[]` while the read is in flight, so a bare `some()` would call every area deleted. */
  it('renders neither a composer nor a verdict while the area list is still loading', async () => {
    let releaseAreas = (): void => undefined;
    const held = new Promise<void>((resolve) => { releaseAreas = () => { resolve(); }; });
    harness({ templates: TEMPLATES, path: '/area/c9/new', heldAreas: held });

    /* Without this the absence checks below would pass against an app that had not rendered at all. */
    await screen.findByRole('button', { name: 'Go to Today' });
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c9/new`);

    expect(screen.queryByLabelText(TASK_LABEL)).toBeNull();
    expect(screen.queryByRole('button', { name: 'Create track' })).toBeNull();
    expect(screen.queryByRole('alert')).toBeNull();

    releaseAreas();
    await held;
  });

  /* `areas` falls back to `[]` on failure with `areasLoading` false, so "has loading stopped" would read a 500 as a landed list. */
  it('reports a failed area read instead of a composer or a deletion verdict', async () => {
    harness({ templates: TEMPLATES, path: '/area/c1/new', areasFail: true });

    /* Scoped to `main`: the rail reports the same failed read in its own ErrorBox.
           `waitFor`, because the failure needs a tick to land. */
    const main = await screen.findByRole('main');
    await waitFor(() => {
      expect(within(main).getByRole('alert').textContent).toContain('areas are unreadable');
    });
    // Not the deletion wording: the server never said this area is gone.
    expect(within(main).getByRole('alert').textContent).not.toContain('This area could not be found.');
    expect(within(main).getByRole('button', { name: 'Retry' })).toBeTruthy();
    expect(screen.queryByLabelText(TASK_LABEL)).toBeNull();
    expect(screen.queryByRole('button', { name: 'Create track' })).toBeNull();
  });
});

describe('the sentence is delivered by the create, and the track opens on it', () => {
  it('sends the sentence once, on the create, and by no second write', async () => {
    const { sent } = harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`));
    expect(createdTrackBodies(sent)[0]).toMatchObject({ first_message: 'Read it' });
    expect(createdTrackRequests(sent)[0]?.headers?.['Idempotency-Key']).toBeDefined();
    /* The `waitFor` above returns the instant the first create is seen; the landing
           is the settle window, so the count is read after the track page mounts. */
    await findTrackPage();
    expect(createdTrackBodies(sent)).toHaveLength(1);
    expect(plannerInputTexts(sent)).toEqual([]);
  });

  it('opens the track\'s planner conversation on arrival', async () => {
    harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`));
    /* Named, not by bare role: the track page's panel column is a `complementary` too. */
    expect(await screen.findByRole('complementary', { name: 'Planner' })).toBeTruthy();
  });

  /* The kernel writes the sentence to the transcript when the queue drains it, so the
       planner card's item read answers with it; nothing is minted on the client. */
  it('shows the sentence that made the track from the row the kernel writes at drain', async () => {
    const { sent } = harness({
      templates: TEMPLATES,
      plannerItems: [{
        id: 1, worker_session_id: 'r', card_id: 'card-planner', track_id: 'w-new', thread_id: 't',
        turn_id: null, item_uuid: 'entry-0001', item_type: 'userMessage', method: 'item/completed',
        params: JSON.stringify({
          item: { id: 'entry-0001', clientId: 'entry-0001', type: 'userMessage', content: [{ type: 'text', text: 'User says:\nRead it' }] },
          _projection: true,
        }),
        input_segments: [{ presentation: 'user', text: 'User says:\nRead it', attachments: [] }],
        created_at_ms: 1,
      }],
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`));
    const drawer = await screen.findByRole('complementary', { name: 'Planner' });
    await waitFor(() => expect(
      [...drawer.querySelectorAll('[data-nc-turn="you"]')].map((turn) => turn.textContent),
    ).toEqual(['Read it']));
    expect(drawer.querySelector('[data-nc-thread-empty]')).toBeNull();
    const items = sent.filter((request) => request.path.includes('/harness/items'));
    expect(items.length).toBeGreaterThan(0);
    expect(plannerInputTexts(sent)).toEqual([]);
  });

  it('shows nothing for the first sentence until the kernel has written its row', async () => {
    harness({ templates: TEMPLATES });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`));
    const drawer = await screen.findByRole('complementary', { name: 'Planner' });
    await waitFor(() => expect(drawer.querySelector('[data-nc-thread-empty]')).not.toBeNull());
    expect(drawer.querySelectorAll('[data-nc-turn="you"]')).toHaveLength(0);
  });

  it('navigates before the track detail lands, and opens the drawer when it does', async () => {
    let releaseDetail = (): void => undefined;
    const held = new Promise<void>((resolve) => { releaseDetail = () => { resolve(); }; });
    const { sent } = harness({ heldDetail: held });

    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    await waitFor(() => { expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`); });
    expect(createdTrackBodies(sent)).toHaveLength(1);
    await findTrackPage();
    expect(screen.queryByRole('complementary', { name: 'Planner' })).toBeNull();

    releaseDetail();
    await held;
    expect(await screen.findByRole('complementary', { name: 'Planner' })).toBeTruthy();
  }, 10_000);

  it('does not yank the reader back when the create lands after they left', async () => {
    let releaseCreate = (): void => undefined;
    const held = new Promise<void>((resolve) => { releaseCreate = () => { resolve(); }; });
    const { sent } = harness({ heldCreate: held });

    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    await userEvent.click(await screen.findByRole('button', { name: 'Go to Today' }));
    await waitFor(() => { expect(window.location.pathname).toBe(`${APP_BASEPATH}/`); });

    releaseCreate();
    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/`);

    /* A leftover intent would surface on the next visit to the track, so that visit is the observable. */
    window.history.pushState({}, '', `${APP_BASEPATH}/track/w-new`);
    /* The title only renders once the track detail has landed, the same read the
           drawer needs; asserting absence before that is vacuous. */
    await screen.findByRole('button', { name: 'Rename track' });
    expect(screen.queryByRole('complementary', { name: 'Planner' })).toBeNull();
  }, 10_000);

  it('still lands on the track when the track detail read fails', async () => {
    const { sent } = harness({
      trackDetail: { status: 500, statusText: 'Server Error', body: { error: 'boom' } },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    await waitFor(() => expect(createdTrackBodies(sent)).toHaveLength(1));
    await waitFor(() => expect(window.location.pathname).toBe(`${APP_BASEPATH}/track/w-new`));
  });

  /* A failed harness start answers 500 *after* the track is minted, so the state of
       the server is the kernel's to say, not this test's. */
  it('reports a create that failed, keeps the sentence, and does not automatically retry', async () => {
    const { sent } = harness({
      trackCreate: { status: 500, statusText: 'Server Error', body: { error: 'boom' } },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    expect(await screen.findByRole('alert')).toBeTruthy();
    expect(window.location.pathname).toBe(`${APP_BASEPATH}/area/c2/new`);
    expect(composerText()).toBe('Read it');
    expect(createdTrackBodies(sent)).toHaveLength(1);
    expect(plannerInputTexts(sent)).toEqual([]);

    /* An explicit retry is the same draft and therefore the same key. */
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(2));
    const [first, retry] = createdTrackRequests(sent);
    expect(retry?.headers?.['Idempotency-Key']).toBe(first?.headers?.['Idempotency-Key']);
  });

  it('rekeys an exhausted draft before the reader explicitly retries it', async () => {
    const { sent } = harness({
      trackCreate: {
        status: 409,
        statusText: 'Conflict',
        body: { error: 'this key is used up', code: 'idempotency_key_exhausted' },
      },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    expect((await screen.findByRole('alert')).textContent).toContain('this key is used up');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(2));
    const [exhausted, fresh] = createdTrackRequests(sent);
    expect(fresh?.headers?.['Idempotency-Key']).toBeDefined();
    expect(fresh?.headers?.['Idempotency-Key'])
      .not.toBe(exhausted?.headers?.['Idempotency-Key']);
    expect(createdTrackBodies(sent)).toEqual([
      expect.objectContaining({ first_message: 'Read it' }),
      expect.objectContaining({ first_message: 'Read it' }),
    ]);
  });

  it.each([
    ['payload conflict', 'already used with different payload'],
    ['legacy unprovable key', 'this key predates durable request fingerprints'],
  ])('does not silently rekey a %s', async (_case, errorMessage) => {
    const { sent } = harness({
      trackCreate: {
        status: 409,
        statusText: 'Conflict',
        body: { error: errorMessage, code: 'conflict' },
      },
    });
    await userEvent.click(await screen.findByRole('button', { name: 'New track in Reading' }));
    await findComposer();
    await userEvent.click(screen.getByLabelText(TASK_LABEL));
    await userEvent.type(screen.getByLabelText(TASK_LABEL), 'Read it');
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));

    expect((await screen.findByRole('alert')).textContent).toContain(errorMessage);
    expect(screen.getByRole('button', { name: 'Start as a new track' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(2));
    const [conflict, retry] = createdTrackRequests(sent);
    expect(retry?.headers?.['Idempotency-Key']).toBe(conflict?.headers?.['Idempotency-Key']);

    await userEvent.click(await screen.findByRole('button', { name: 'Start as a new track' }));
    expect(screen.queryByRole('alert')).toBeNull();
    expect(createdTrackRequests(sent)).toHaveLength(2);
    await userEvent.click(screen.getByRole('button', { name: 'Create track' }));
    await waitFor(() => expect(createdTrackRequests(sent)).toHaveLength(3));
    const explicitNew = createdTrackRequests(sent)[2];
    expect(explicitNew?.headers?.['Idempotency-Key']).toBeDefined();
    expect(explicitNew?.headers?.['Idempotency-Key'])
      .not.toBe(conflict?.headers?.['Idempotency-Key']);
  });
});
