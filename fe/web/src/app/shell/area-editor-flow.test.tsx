// @vitest-environment jsdom

import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { AREA_PALETTE } from '../../features/area/palette.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { AppShell } from './public.tsx';
import { ApiError } from '../../../../core/domain/failure-class.ts';
import { AreaCreatePreflightError } from '../providers/queries.ts';
import type { ApiRequest, ApiTransportPort, ApiTransportResponse } from '../../../../core/api/types.ts';

const harness = vi.hoisted(() => ({
  compact: false,
  realMutations: false,
  area: {
    id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user' as const,
    defaultTemplateId: null as string | null,
    defaultCwd: null as string | null,
    createdAt: 1,
    updatedAt: 1,
  },
  templates: [{ id: 'small-change', title: 'Small change', tasks: [] }] as {
    id: string; title: string; tasks: { key: string; goal: string }[];
  }[],
  templatesLoaded: true,
  templatesError: null as string | null,
  create: vi.fn(),
  update: vi.fn(),
  remove: vi.fn(),
}));

vi.mock('@tanstack/react-router', () => ({
  Outlet: () => <div>route</div>,
  useNavigate: () => vi.fn(),
  useRouter: () => ({}),
  useRouterState: () => undefined,
}));

vi.mock('../providers/queries.ts', async (importOriginal) => {
  const original = await importOriginal<typeof import('../providers/queries.ts')>();
  return {
    ...original,
    useWorkspace: () => ({
      areas: [harness.area],
      tracks: [],
      tracksByArea: new Map([['c1', []]]),
      areasLoading: false,
      overlaysLoading: false,
      areasError: null,
      overlaysError: null,
      trackErrorsByArea: new Map(),
      tracksLoadingByArea: new Map(),
      retryAreas: vi.fn(),
      retryOverlays: vi.fn(),
      retryTracks: vi.fn(),
    }),
    useAreaMutations: (...args: Parameters<typeof original.useAreaMutations>) => {
      const mutations = original.useAreaMutations(...args);
      return harness.realMutations ? mutations : {
        create: harness.create,
        update: harness.update,
        remove: harness.remove,
      };
    },
    useTrackMutations: () => ({
      create: vi.fn(), patch: vi.fn(), setPinned: vi.fn(), createTerminal: vi.fn(),
      createCodex: vi.fn(), createCard: vi.fn(), removeCard: vi.fn(), remove: vi.fn(),
    }),
    useTrackTemplates: () => ({
      templates: harness.templates,
      loaded: harness.templatesLoaded,
      error: harness.templatesError,
      refetch: vi.fn(),
    }),
  };
});

vi.mock('../router/navigation.ts', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../router/navigation.ts')>()),
  useCurrentPath: () => '/',
  useGo: () => vi.fn(),
  useTrackPanelNavigation: () => ({ closePanel: vi.fn() }),
  routeParamFromPath: () => undefined,
}));

vi.mock('../../ui/viewport/public.ts', () => ({
  useCompactViewport: () => harness.compact,
}));

vi.mock('./settings-overlay.tsx', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./settings-overlay.tsx')>()),
  SettingsOverlay: () => null,
}));

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

function memoryStorage() {
  const values = new Map<string, string>();
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
  };
}

function renderShell(compact = false, transport: ApiTransportPort = { send: vi.fn() }) {
  harness.compact = compact;
  return render(
    <QueryClientProvider client={new QueryClient({ defaultOptions: { mutations: { retry: false } } })}>
      <ThemeProvider storage={memoryStorage()}>
        <AppShell
          transport={transport}
          unauthorized={unauthorized}
          onOpenSettings={vi.fn()}
          onOpenPlugins={vi.fn()}
          onSignOut={vi.fn()}
        />
      </ThemeProvider>
    </QueryClientProvider>,
  );
}

async function openDesktopEditor(): Promise<void> {
  await userEvent.click(screen.getByRole('button', { name: 'Area actions for Work' }));
  await userEvent.click(screen.getByRole('menuitem', { name: 'Edit area' }));
}

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

beforeEach(() => {
  harness.compact = false;
  harness.realMutations = false;
  harness.area = {
    ...harness.area,
    name: 'Work',
    defaultTemplateId: null,
    defaultCwd: null,
  };
  harness.templates = [{ id: 'small-change', title: 'Small change', tasks: [] }];
  harness.templatesLoaded = true;
  harness.templatesError = null;
  harness.create.mockReset().mockResolvedValue(harness.area);
  harness.update.mockReset().mockResolvedValue(harness.area);
  harness.remove.mockReset().mockResolvedValue(undefined);
  vi.stubGlobal('matchMedia', vi.fn(() => ({
    matches: false,
    addEventListener: vi.fn(),
    removeEventListener: vi.fn(),
  })));
});

describe('AppShell Area editor flow', () => {
  function areaTransport(replies: (() => ApiTransportResponse)[]) {
    const writes: ApiRequest[] = [];
    const transport: ApiTransportPort = { send: (request) => {
      if (request.path === '/api/agent-providers') {
        return Promise.resolve({ status: 200, statusText: 'OK', body: [] });
      }
      if (request.path === '/api/version') {
        return Promise.resolve({ status: 200, statusText: 'OK', body: { areaCreateIdempotency: true } });
      }
      writes.push(request);
      const reply = replies.shift();
      if (reply === undefined) throw new Error('Unexpected Area write');
      return Promise.resolve(reply());
    } };
    harness.realMutations = true;
    renderShell(false, transport);
    return writes;
  }

  const tooLarge = () => ({ status: 413, statusText: 'Payload Too Large', body: 'request too large' });
  const success = () => ({ status: 201, statusText: 'Created', body: {
    id: 'new', name: 'Corrected', color: '#5B8DEF', sort: 1, kind: 'user', created_at: 1, updated_at: 1,
  } });

  it('releases a first create 413 and submits the corrected body under a new key', async () => {
    const writes = areaTransport([tooLarge, success]);
    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    const input = screen.getByRole<HTMLInputElement>('textbox', { name: /^Name/ });
    await userEvent.type(input, 'Too large');
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));
    expect((await screen.findByRole('alert')).textContent).toContain('Payload Too Large');
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
    expect(input.disabled).toBe(false);
    await userEvent.clear(input);
    await userEvent.type(input, 'Corrected');
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'New area' })).toBeNull());
    expect(writes).toHaveLength(2);
    expect(writes[0]?.headers?.['Idempotency-Key']).toEqual(expect.any(String));
    expect(writes[1]?.headers?.['Idempotency-Key']).not.toBe(writes[0]?.headers?.['Idempotency-Key']);
    expect(writes.map((request) => request.body)).toEqual([
      expect.objectContaining({ name: 'Too large' }), expect.objectContaining({ name: 'Corrected' }),
    ]);
  });

  it('keeps the original create identity after an unknown attempt followed by 413', async () => {
    const writes = areaTransport([() => { throw new Error('Response lost'); }, tooLarge, success]);
    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    const input = screen.getByRole<HTMLInputElement>('textbox', { name: /^Name/ });
    await userEvent.type(input, 'Keep original');
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));
    expect((await screen.findByRole('alert')).textContent).toContain('Creation could not be confirmed');
    expect(input.disabled).toBe(true);
    await userEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('Payload Too Large'));
    expect(screen.getByRole('alert').textContent).toContain('Creation could not be confirmed');
    expect(input.disabled).toBe(true);
    await userEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'New area' })).toBeNull());
    expect(writes).toHaveLength(3);
    expect(writes[0]?.headers?.['Idempotency-Key']).toEqual(expect.any(String));
    expect(writes[1]).toEqual(writes[0]);
    expect(writes[2]).toEqual(writes[0]);
  });

  it.each([
    [413, 'Payload Too Large'], [499, 'The area update is unconfirmed.'],
  ])('keeps PATCH %i semantics through the transport', async (status, text) => {
    const writes = areaTransport([() => ({ status, statusText: 'Payload Too Large', body: '' })]);
    await openDesktopEditor();
    const input = screen.getByRole<HTMLInputElement>('textbox', { name: /^Name/ });
    await userEvent.clear(input);
    await userEvent.type(input, 'Changed');
    await userEvent.click(screen.getByRole('button', { name: 'Save changes' }));
    expect((await screen.findByRole('alert')).textContent).toContain(text);
    expect(writes).toHaveLength(1);
    expect(writes[0]).toMatchObject({ method: 'PATCH', path: '/api/areas/c1', body: { name: 'Changed' } });
    expect(input.disabled).toBe(false);
  });

  it('creates from the shared Dialog with the two pill values', async () => {
    renderShell();
    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    const dialog = screen.getByRole('dialog', { name: 'New area' });
    expect(within(dialog).queryByText('New area')).toBeNull();
    expect(within(dialog).queryByText('Required')).toBeNull();
    const name = within(dialog).getByRole<HTMLInputElement>('textbox', { name: 'Name' });
    expect(name.required).toBe(true);
    await userEvent.type(name, 'Reading');
    await userEvent.click(screen.getByRole('button', { name: 'Default template: No template' }));
    await userEvent.click(screen.getByRole('menuitem', { name: /^Small change/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));

    await waitFor(() => expect(harness.create).toHaveBeenCalledTimes(1));
    const body = harness.create.mock.calls[0]?.[0] as Record<string, unknown>;
    expect(body).toMatchObject({
      name: 'Reading', default_template_id: 'small-change', default_cwd: null,
    });
    expect(AREA_PALETTE).toContain(body.color);
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'New area' })).toBeNull());
  });

  it('retries a lost create confirmation with the same identity and payload after reopening', async () => {
    harness.create.mockRejectedValueOnce(new Error('Transport request failed'))
      .mockRejectedValueOnce(new AreaCreatePreflightError('Version read failed.'));
    renderShell();
    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    await userEvent.type(screen.getByRole('textbox', { name: /^Name/ }), 'Keep one');
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));
    await screen.findByRole('alert');
    const [body, key] = harness.create.mock.calls[0] as [unknown, string];
    expect(key).toEqual(expect.any(String));
    expect(key.length).toBeGreaterThan(0);
    expect(screen.getByRole<HTMLInputElement>('textbox', { name: /^Name/ }).disabled).toBe(true);
    await userEvent.keyboard('{Escape}');
    expect(screen.queryByRole('dialog', { name: 'New area' })).toBeNull();
    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    expect(screen.getByRole<HTMLInputElement>('textbox', { name: /^Name/ }).value).toBe('Keep one');
    await userEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(harness.create).toHaveBeenCalledTimes(2));
    expect(harness.create.mock.calls[1]).toEqual([body, key]);
    expect((await screen.findByRole('alert')).textContent).toContain('Version read failed.');
    expect(screen.getByRole<HTMLInputElement>('textbox', { name: /^Name/ }).disabled).toBe(true);
    await userEvent.click(screen.getByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(harness.create).toHaveBeenCalledTimes(3));
    expect(harness.create.mock.calls[2]).toEqual([body, key]);
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'New area' })).toBeNull());
    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    await userEvent.type(screen.getByRole('textbox', { name: /^Name/ }), 'Keep one');
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));
    await waitFor(() => expect(harness.create).toHaveBeenCalledTimes(4));
    expect(harness.create.mock.calls[3]?.[1]).not.toBe(key);
  });

  /* #2068: a key bound to another request can never be answered for this one, so the draft is not
     offered a retry under it; the next Create is a new request under a new key. */
  it('offers no retry after a key-reused answer and creates anew under a fresh key', async () => {
    harness.create.mockRejectedValueOnce(new ApiError({
      kind: 'http', status: 409, code: 'idempotency_key_reused', message: 'This Area creation key belongs to a different request.',
    }));
    renderShell();
    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    await userEvent.type(screen.getByRole('textbox', { name: /^Name/ }), 'Reused');
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain('belongs to a different request');
    expect(alert.textContent).not.toContain('Try again');
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
    expect(screen.getByRole<HTMLInputElement>('textbox', { name: /^Name/ }).disabled).toBe(false);
    const reusedKey: unknown = harness.create.mock.calls[0]?.[1];
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));
    await waitFor(() => expect(harness.create).toHaveBeenCalledTimes(2));
    expect(harness.create.mock.calls[1]?.[1]).not.toBe(reusedKey);
  });

  /* A spent key ends the request even after an unconfirmed attempt: the Area it made was deleted. */
  it('drops an unconfirmed request whose retry finds its Area deleted', async () => {
    harness.create.mockRejectedValueOnce(new Error('Transport request failed'))
      .mockRejectedValueOnce(new ApiError({
        kind: 'http', status: 409, code: 'idempotency_key_exhausted', message: 'The Area created by this request was deleted.',
      }));
    renderShell();
    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    await userEvent.type(screen.getByRole('textbox', { name: /^Name/ }), 'Deleted');
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Try again' }));
    await waitFor(() => expect(harness.create).toHaveBeenCalledTimes(2));
    const alert = await screen.findByRole('alert');
    await waitFor(() => expect(alert.textContent).toContain('was deleted'));
    expect(screen.queryByRole('button', { name: 'Try again' })).toBeNull();
    const spentKey: unknown = harness.create.mock.calls[1]?.[1];
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));
    await waitFor(() => expect(harness.create).toHaveBeenCalledTimes(3));
    expect(harness.create.mock.calls[2]?.[1]).not.toBe(spentKey);
  });

  it('keeps an unsubmitted preflight failure editable and permits an explicit discard', async () => {
    harness.create.mockRejectedValueOnce(new AreaCreatePreflightError('Update the server.'));
    renderShell();
    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    await userEvent.type(screen.getByRole('textbox', { name: /^Name/ }), 'Before');
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));
    await screen.findByRole('alert');
    const input = screen.getByRole<HTMLInputElement>('textbox', { name: /^Name/ });
    expect(input.disabled).toBe(false);
    await userEvent.clear(input);
    await userEvent.type(input, 'Corrected');
    harness.create.mockRejectedValueOnce(new Error('Response lost'));
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));
    const discard = await screen.findByRole('button', { name: 'Discard draft' });
    const unresolvedKey: unknown = harness.create.mock.calls[1]?.[1];
    await userEvent.click(discard);
    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    expect(screen.getByRole<HTMLInputElement>('textbox', { name: /^Name/ }).value).toBe('');
    await userEvent.type(screen.getByRole('textbox', { name: /^Name/ }), 'Corrected');
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));
    await waitFor(() => expect(harness.create).toHaveBeenCalledTimes(3));
    expect(harness.create.mock.calls[2]?.[1]).not.toBe(unresolvedKey);
  });

  it('sends only a changed name when an unavailable saved template is untouched', async () => {
    harness.area = {
      ...harness.area, defaultTemplateId: 'retired-template', defaultCwd: '/srv/ legal-space ',
    };
    harness.templates = [];
    harness.templatesLoaded = false;
    harness.templatesError = 'Could not load templates.';
    renderShell();
    await openDesktopEditor();
    const name = screen.getByRole<HTMLInputElement>('textbox', { name: /^Name/ });
    await userEvent.clear(name);
    await userEvent.type(name, 'Studio');
    await userEvent.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => expect(harness.update).toHaveBeenCalledWith('c1', { name: 'Studio' }));
  });

  it('sends explicit nulls when one Track default is cleared at the Area', async () => {
    harness.area = {
      ...harness.area, defaultTemplateId: 'small-change', defaultCwd: '/srv/work',
    };
    renderShell();
    await openDesktopEditor();
    await userEvent.click(screen.getByRole('button', { name: 'Default template: Small change' }));
    await userEvent.click(screen.getByRole('menuitem', { name: /^No template/ }));
    await userEvent.click(screen.getByRole('button', { name: 'Use a new Neige workspace' }));
    await userEvent.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => expect(harness.update).toHaveBeenCalledWith('c1', {
      default_template_id: null,
      default_cwd: null,
    }));
  });

  it('keeps the draft open on failure and presents a truthful busy modal', async () => {
    let settle!: (value: typeof harness.area) => void;
    harness.create.mockReturnValueOnce(new Promise((resolve) => { settle = resolve; }));
    renderShell();
    await userEvent.click(screen.getByRole('button', { name: 'New area' }));
    await userEvent.type(screen.getByRole('textbox', { name: /^Name/ }), 'Still here');
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));

    const busy = await screen.findByRole<HTMLButtonElement>('button', { name: 'Saving…' });
    expect(busy.getAttribute('aria-busy')).toBe('true');
    expect(busy.getAttribute('aria-disabled')).toBe('true');
    expect(busy.disabled).toBe(false);
    expect(document.activeElement).toBe(busy);
    expect(screen.queryByRole('button', { name: 'Close' })).toBeNull();
    expect(screen.getByRole<HTMLButtonElement>('button', { name: 'Cancel' }).disabled).toBe(true);
    await userEvent.keyboard('{Escape}');
    expect(screen.getByRole('dialog', { name: 'New area' })).toBeTruthy();

    settle(harness.area);
    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'New area' })).toBeNull());

    harness.create.mockRejectedValueOnce(new Error('Area write failed'));
    await userEvent.click(await screen.findByRole('button', { name: 'New area' }));
    await userEvent.type(screen.getByRole('textbox', { name: /^Name/ }), 'Still here');
    await userEvent.click(screen.getByRole('button', { name: 'Create area' }));
    /* An unknown answer is the fixed state, never the error's own text (#2131). */
    expect((await screen.findByRole('alert')).textContent).toBe('Creation could not be confirmed. Try again to safely check the same area.');
    expect(screen.getByRole<HTMLInputElement>('textbox', { name: /^Name/ }).value).toBe('Still here');
  });

  it('opens the same editor from the centered Area menu and the navigation edit icon', async () => {
    renderShell(true);
    await userEvent.click(screen.getByRole('button', { name: 'Switch area, Work' }));
    await userEvent.click(screen.getByRole('menuitem', { name: 'New area' }));
    expect(screen.getByRole('dialog', { name: 'New area' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    await userEvent.click(screen.getByRole('button', { name: 'Open areas' }));
    const navigation = screen.getByRole('dialog', { name: 'Tracks and settings' });
    await userEvent.click(within(navigation).getByRole('button', { name: 'Work' }));
    expect(within(navigation).getByRole('button', { name: 'Back to Areas' })).toBeTruthy();
    await userEvent.click(within(navigation).getByRole('button', { name: 'Area actions' }));
    await userEvent.click(within(navigation).getByRole('menuitem', { name: 'Edit area Work' }));
    expect(screen.getByRole('dialog', { name: 'Edit Work' })).toBeTruthy();
  });
});
