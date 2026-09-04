// @vitest-environment jsdom

import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { createUnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { AREA_PALETTE } from '../../features/area/palette.ts';
import { ThemeProvider } from '../theme/public.tsx';
import { AppShell } from './public.tsx';

const harness = vi.hoisted(() => ({
  compact: false,
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

vi.mock('../providers/queries.ts', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../providers/queries.ts')>()),
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
  useAreaMutations: () => ({
    create: harness.create,
    update: harness.update,
    remove: harness.remove,
  }),
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
}));

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

vi.mock('./settings-overlay.tsx', () => ({ SettingsOverlay: () => null }));

const unauthorized = createUnauthorizedChannel({ enqueue: (task) => task() });

function memoryStorage() {
  const values = new Map<string, string>();
  return {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => { values.set(key, value); },
  };
}

function renderShell(compact = false) {
  harness.compact = compact;
  return render(
    <ThemeProvider storage={memoryStorage()}>
      <AppShell
        transport={{ send: vi.fn() }}
        unauthorized={unauthorized}
        onOpenSettings={vi.fn()}
        onOpenPlugins={vi.fn()}
        onSignOut={vi.fn()}
      />
    </ThemeProvider>,
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
    expect((await screen.findByRole('alert')).textContent).toContain('Area write failed');
    expect(screen.getByRole<HTMLInputElement>('textbox', { name: /^Name/ }).value).toBe('Still here');
  });

  it('opens the same editor from mobile New and Edit actions', async () => {
    renderShell(true);
    await userEvent.click(screen.getByRole('button', { name: 'Areas' }));
    const areas = screen.getByRole('dialog', { name: 'Areas' });
    await userEvent.click(within(areas).getByRole('button', { name: 'New area' }));
    expect(screen.getByRole('dialog', { name: 'New area' })).toBeTruthy();
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));

    await userEvent.click(within(areas).getByRole('button', { name: /Work/ }));
    await userEvent.click(within(areas).getByRole('button', { name: 'Edit area Work' }));
    expect(screen.getByRole('dialog', { name: 'Edit Work' })).toBeTruthy();
  });
});
