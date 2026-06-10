// Tests for the keyboard-entry rename path on WavePage (slice 3 of #56).
//
// What we lock in here:
//
//   1. The title span is reachable via Tab (tabindex/role wired up).
//   2. Enter on the title span enters edit mode; the <input> renders and
//      receives focus.
//   3. F2 on the title span behaves identically to Enter.
//   4. Escape in edit mode exits to display mode AND returns focus to the
//      title span.
//   5. Enter (commit) in edit mode exits to display mode AND returns focus
//      to the title span, and the rename callback fires with trimmed text.
//
// Mouse-only path remains covered by the existing click handler — we don't
// re-test that here (slice 1 already locks it in via the production code
// path that hasn't changed shape).

import { describe, it, expect, vi } from 'vitest';
import { render, screen, act, fireEvent, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import type { ReactNode } from 'react';
import { WavePage } from './Wave';
import type { Cove, Wave, WaveCardSlot } from '../types';
import * as api from '../api/calm';
import { DARK_THEME_RGB } from '../api/themeRgb';
import type { WaveReportCardData } from '../cards/builtins/wave-report';
import type { KernelOverlay, NewOverlayBody } from '../api/wire';

// WaveGrid is lazy-loaded via React.lazy + an internal dynamic import.
// For these tests we never actually render any cards, but the Suspense
// fallback still needs to resolve. Stub the module to a trivial component.
vi.mock('../WaveGrid', () => ({
  WaveGrid: () => <div data-testid="wave-grid-stub" />,
}));

// WaveList (Slice 9) is lazy-loaded via React.lazy and only used when the
// per-wave view-mode overlay says `list`. The rename tests run in the
// default grid mode, so we stub for completeness only.
vi.mock('../WaveList', () => ({
  WaveList: () => <div data-testid="wave-list-stub" />,
}));

// AddPanel pulls in the full card registry and a heavy menu DOM tree. The
// mock keeps rename tests lightweight and exposes one codex trigger for
// the create-error modal coverage below.
vi.mock('../shared/components/AddPanel', () => ({
  AddPanel: ({
    onSelect,
  }: {
    onSelect: (item: {
      type: string;
      label: string;
      createSchema?: {
        fields: Array<{ key: string; label: string; type: 'directory' }>;
      };
    }) => void;
  }) => (
    <button
      type="button"
      data-testid="add-panel-stub"
      onClick={() =>
        onSelect({
          type: 'codex',
          label: 'codex',
          createSchema: {
            fields: [{ key: 'cwd', label: 'Working directory', type: 'directory' }],
          },
        })
      }
    >
      Add codex
    </button>
  ),
}));

// Mock the calm-server REST client so the view-mode overlay query that
// `WavePage` now mounts (Slice 9) doesn't hit the network in jsdom. It
// resolves to "no overlay rows", which puts the page in its default
// grid mode — matching every existing test's expectation.
vi.mock('../api/calm', async () => {
  const actual = await vi.importActual<typeof import('../api/calm')>(
    '../api/calm',
  );
  return {
    ...actual,
    listOverlays: vi.fn().mockResolvedValue([]),
    upsertOverlay: vi.fn().mockResolvedValue({}),
    listDir: vi.fn().mockResolvedValue({
      path: '/tmp/project',
      parent: '/tmp',
      entries: [],
    }),
    createCodexCard: vi.fn(),
  };
});

// `WavePage` calls `useOverlayState` for the per-wave view-mode toggle
// (Slice 9 of issue #56). The hook reads `useQueryClient()` — without a
// QueryClientProvider every render throws. Wrap each rendered tree.
function makeClient(): QueryClient {
  return new QueryClient({
    defaultOptions: {
      queries: { retry: false, gcTime: 0, staleTime: 0 },
      mutations: { retry: false },
    },
  });
}
function withClient(ui: ReactNode): ReactNode {
  return <QueryClientProvider client={makeClient()}>{ui}</QueryClientProvider>;
}

function makeCove(): Cove {
  return { id: 'c1', name: 'Atlas', subtitle: '', color: '#5a9' };
}

function makeWave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'w1',
    coveId: 'c1',
    title: 'Migrate auth',
    lifecycle: 'draft',
    anyCardNeedsInput: false,
    progress: 0,
    eta: '',
    now: '',
    // Issue #250 PR 5 — calendar rail needs these on the UI shape;
    // tests pin fixed values so timing-sensitive assertions stay
    // deterministic.
    createdAt: 0,
    terminalAt: null,
    pinnedAt: null,
    cards: [],
    ...overrides,
  };
}

function makeReportSlot(body = 'Report body'): WaveCardSlot {
  const card: WaveReportCardData = {
    type: 'wave-report',
    id: 'report_1',
    summary: '',
    body,
    updatedAt: 2_000,
  };
  return { kind: 'card', card, sort: -1, deletable: false };
}

function makeViewModeOverlay(mode: 'grid' | 'list' | 'report'): KernelOverlay {
  return {
    id: 'ov-view-mode',
    plugin_id: 'kernel',
    entity_kind: 'view',
    entity_id: 'w1',
    kind: 'view-mode',
    payload: { schemaVersion: 1, mode },
    updated_at: 0,
  };
}

function echoOverlay(body: NewOverlayBody): KernelOverlay {
  return {
    id: 'ov-view-mode',
    plugin_id: body.plugin_id,
    entity_kind: body.entity_kind,
    entity_id: body.entity_id,
    kind: body.kind,
    payload: body.payload,
    updated_at: 0,
  };
}

describe('WavePage rename keyboard entry', () => {
  it('exposes the wave title as a focusable button named after the wave', () => {
    render(
      withClient(
        <WavePage
          wave={makeWave()}
          cove={makeCove()}
          onGo={() => {}}
          onAddCard={() => {}}
          onRemoveCard={() => {}}
          onRenameWave={() => {}}
        />,
      ),
    );
    const title = screen.getByRole('button', { name: 'Migrate auth' });
    expect(title).toHaveAttribute('tabindex', '0');
    // After #56 followup, the rename verb is conveyed via
    // aria-describedby (not stuffed into aria-label) so the accessible
    // *name* stays "Migrate auth" while the *description* says "Rename wave".
    expect(title).toHaveAccessibleDescription('Rename wave');
  });

  it('renders the fallback label when the wave title is empty', () => {
    render(
      withClient(
        <WavePage
          wave={makeWave({ title: '' })}
          cove={makeCove()}
          onGo={() => {}}
          onAddCard={() => {}}
          onRemoveCard={() => {}}
          onRenameWave={() => {}}
        />,
      ),
    );
    const title = screen.getByRole('button', { name: 'Untitled wave' });
    expect(title).toHaveTextContent('Untitled wave');
    expect(title).toHaveAccessibleDescription('Rename wave');
  });

  it('drops the keyboard affordance entirely when onRenameWave is absent', () => {
    render(
      withClient(
        <WavePage
          wave={makeWave()}
          cove={makeCove()}
          onGo={() => {}}
          onAddCard={() => {}}
          onRemoveCard={() => {}}
          // no onRenameWave
        />,
      ),
    );
    // Title still renders as plain text, but it shouldn't be a button.
    expect(screen.getByText('Migrate auth')).not.toHaveAttribute('role', 'button');
  });

  it('Enter on the title span opens rename mode and focuses the input', async () => {
    const user = userEvent.setup();
    render(
      withClient(
        <WavePage
          wave={makeWave()}
          cove={makeCove()}
          onGo={() => {}}
          onAddCard={() => {}}
          onRemoveCard={() => {}}
          onRenameWave={() => {}}
        />,
      ),
    );
    const title = screen.getByRole('button', { name: 'Migrate auth' });
    title.focus();
    expect(document.activeElement).toBe(title);

    await user.keyboard('{Enter}');
    // queueMicrotask runs immediately after the current microtask;
    // userEvent.keyboard awaits it. The input should now exist and be
    // focused.
    const input = screen.getByRole('textbox', { name: 'Wave title' });
    expect(input).toBeInTheDocument();
    expect(document.activeElement).toBe(input);
  });

  it('F2 on the title span opens rename mode (parity with Enter)', async () => {
    const user = userEvent.setup();
    render(
      withClient(
        <WavePage
          wave={makeWave()}
          cove={makeCove()}
          onGo={() => {}}
          onAddCard={() => {}}
          onRemoveCard={() => {}}
          onRenameWave={() => {}}
        />,
      ),
    );
    const title = screen.getByRole('button', { name: 'Migrate auth' });
    title.focus();
    await user.keyboard('{F2}');
    expect(screen.getByRole('textbox', { name: 'Wave title' })).toBeInTheDocument();
  });

  it('Escape exits rename mode and restores focus to the title display', async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(
      withClient(
        <WavePage
          wave={makeWave()}
          cove={makeCove()}
          onGo={() => {}}
          onAddCard={() => {}}
          onRemoveCard={() => {}}
          onRenameWave={onRename}
        />,
      ),
    );
    const title = screen.getByRole('button', { name: 'Migrate auth' });
    title.focus();
    await user.keyboard('{Enter}');
    const input = screen.getByRole('textbox', { name: 'Wave title' });
    // Type something that we expect to *not* be saved on cancel.
    await user.type(input, ' new');
    await user.keyboard('{Escape}');

    // Display mode is back, no save fired.
    expect(screen.queryByRole('textbox', { name: 'Wave title' })).not.toBeInTheDocument();
    expect(onRename).not.toHaveBeenCalled();
    // Focus returned to the display element.
    const restored = screen.getByRole('button', { name: 'Migrate auth' });
    expect(document.activeElement).toBe(restored);
  });

  it('Enter commits a renamed value and restores focus to the title display', async () => {
    const user = userEvent.setup();
    const onRename = vi.fn();
    render(
      withClient(
        <WavePage
          wave={makeWave()}
          cove={makeCove()}
          onGo={() => {}}
          onAddCard={() => {}}
          onRemoveCard={() => {}}
          onRenameWave={onRename}
        />,
      ),
    );
    const title = screen.getByRole('button', { name: 'Migrate auth' });
    title.focus();
    await user.keyboard('{Enter}');
    const input = screen.getByRole('textbox', { name: 'Wave title' });
    // Drive the input via fireEvent so the controlled-input lifecycle
    // around the Enter-driven commit re-render is deterministic (see
    // matching note in Cove.test.tsx — userEvent's per-character path
    // raced the setEditing(false) → useEffect → focus-restore flush).
    fireEvent.change(input, { target: { value: 'New plan' } });
    fireEvent.keyDown(input, { key: 'Enter' });

    await act(async () => {
      await Promise.resolve();
    });

    expect(onRename).toHaveBeenCalledWith('w1', 'New plan');
    // Focus restoration: since the parent in production would re-render
    // with the new title, but in this test we keep wave.title unchanged,
    // the display span still appears with the original label.
    const restored = screen.getByRole('button', { name: 'Migrate auth' });
    expect(document.activeElement).toBe(restored);
  });
});

describe('WavePage schema card create errors', () => {
  it('shows a codex create 500 inline and keeps the modal open', async () => {
    const user = userEvent.setup();
    vi.mocked(api.listDir).mockResolvedValue({
      path: '/tmp/project',
      parent: '/tmp',
      entries: [],
    });
    vi.mocked(api.createCodexCard).mockRejectedValueOnce(
      new api.CalmApiError(
        500,
        'internal',
        'internal: shared codex app-server is not running',
      ),
    );

    render(
      withClient(
        <WavePage
          wave={makeWave()}
          cove={makeCove()}
          onGo={() => {}}
          onAddCard={() => {}}
          onCreateCardWithBody={async (waveId, _type, values) => {
            await api.createCodexCard(waveId, {
              cwd: values.cwd,
              theme: DARK_THEME_RGB,
            });
          }}
          onRemoveCard={() => {}}
          onRenameWave={() => {}}
        />,
      ),
    );

    await user.click(screen.getByRole('button', { name: 'Add codex' }));
    const createHere = await screen.findByRole('button', { name: 'Create here' });
    await waitFor(() => expect(createHere).not.toBeDisabled());
    await user.click(createHere);

    expect(await screen.findByRole('alert')).toHaveTextContent(
      'internal: shared codex app-server is not running',
    );
    expect(screen.getByRole('dialog', { name: 'New codex' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Create here' })).toBeInTheDocument();
    expect(api.createCodexCard).toHaveBeenCalledWith('w1', {
      cwd: '/tmp/project',
      theme: DARK_THEME_RGB,
    });
  });
});

describe('WavePage report view mode', () => {
  it('defaults to grid when the wave has a report card and no overlay', () => {
    render(
      withClient(
        <WavePage
          wave={makeWave({ cards: [makeReportSlot('Default report body')] })}
          cove={makeCove()}
          onGo={() => {}}
          onAddCard={() => {}}
          onRemoveCard={() => {}}
          onRenameWave={() => {}}
        />,
      ),
    );

    expect(screen.getByTestId('wave-grid-stub')).toBeInTheDocument();
    expect(screen.getByTestId('add-panel-stub')).toBeInTheDocument();
    expect(
      screen.getByRole('switch', { name: 'Switch wave to report view' }),
    ).toBeInTheDocument();
    expect(screen.queryByText('Default report body')).not.toBeInTheDocument();
  });

  it('does not render AddPanel while in explicit report mode', async () => {
    vi.mocked(api.listOverlays).mockResolvedValueOnce([
      makeViewModeOverlay('report'),
    ]);

    render(
      withClient(
        <WavePage
          wave={makeWave({ cards: [makeReportSlot()] })}
          cove={makeCove()}
          onGo={() => {}}
          onAddCard={() => {}}
          onRemoveCard={() => {}}
          onRenameWave={() => {}}
        />,
      ),
    );

    expect(await screen.findByText('Report body')).toBeInTheDocument();
    expect(screen.queryByTestId('add-panel-stub')).not.toBeInTheDocument();
  });

  it('renders the report empty state when explicit report mode has no report card', async () => {
    vi.mocked(api.listOverlays).mockResolvedValueOnce([
      makeViewModeOverlay('report'),
    ]);

    render(
      withClient(
        <WavePage
          wave={makeWave()}
          cove={makeCove()}
          onGo={() => {}}
          onAddCard={() => {}}
          onRemoveCard={() => {}}
          onRenameWave={() => {}}
        />,
      ),
    );

    expect(
      await screen.findByText(
        'Report not ready. The spec agent has not produced a report yet.',
      ),
    ).toBeInTheDocument();
    expect(screen.queryByRole('switch')).not.toBeInTheDocument();
    expect(screen.queryByTestId('add-panel-stub')).not.toBeInTheDocument();
  });

  it('hides the report toggle for worker-only waves', () => {
    render(
      withClient(
        <WavePage
          wave={makeWave()}
          cove={makeCove()}
          onGo={() => {}}
          onAddCard={() => {}}
          onRemoveCard={() => {}}
          onRenameWave={() => {}}
        />,
      ),
    );

    expect(screen.queryByRole('switch')).not.toBeInTheDocument();
    expect(screen.queryByText(/Report not ready/)).not.toBeInTheDocument();
    expect(screen.getByTestId('add-panel-stub')).toBeInTheDocument();
  });

  it('writes report and cards mode changes to the view-mode overlay', async () => {
    const user = userEvent.setup();
    vi.mocked(api.upsertOverlay).mockClear();
    vi.mocked(api.upsertOverlay)
      .mockImplementationOnce(async (body) => echoOverlay(body))
      .mockImplementationOnce(async (body) => echoOverlay(body));

    render(
      withClient(
        <WavePage
          wave={makeWave({ cards: [makeReportSlot()] })}
          cove={makeCove()}
          onGo={() => {}}
          onAddCard={() => {}}
          onRemoveCard={() => {}}
          onRenameWave={() => {}}
        />,
      ),
    );

    await user.click(
      screen.getByRole('switch', { name: 'Switch wave to report view' }),
    );

    expect(api.upsertOverlay).toHaveBeenCalledWith({
      plugin_id: 'kernel',
      entity_kind: 'view',
      entity_id: 'w1',
      kind: 'view-mode',
      payload: { schemaVersion: 1, mode: 'report' },
    });

    await user.click(
      await screen.findByRole('switch', { name: 'Switch wave to cards view' }),
    );

    expect(api.upsertOverlay).toHaveBeenLastCalledWith({
      plugin_id: 'kernel',
      entity_kind: 'view',
      entity_id: 'w1',
      kind: 'view-mode',
      payload: { schemaVersion: 1, mode: 'grid' },
    });
  });
});
