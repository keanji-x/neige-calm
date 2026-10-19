// Shared fixtures for the TrackPage behaviour and contract suites.

import { render, type RenderResult } from '@testing-library/react';
import { vi } from 'vitest';

import type { ReportTaskRow } from '../../../../../core/domain/report.ts';
import { NEUTRAL_ACTIVITY, type CardWire, type Track } from '../../../../../core/domain/track.ts';
import { useState } from '../../../ui/state/public.ts';
import { TrackPage, type TrackPageProps } from './public.tsx';

type Panel = NonNullable<TrackPageProps['panel']>;

export function track(overrides: Partial<Track> = {}): Track {
  return {
    id: 'w1', areaId: 'c1', title: 'Alpha', sort: 1, cwd: '/tmp/alpha', isolation: 'none', agentCwd: '/tmp/alpha',
    pinnedAt: null, closedAt: null, createdAt: 0, updatedAt: 0,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

export function card(overrides: Partial<CardWire> = {}): CardWire {
  return {
    id: 'card-1', track_id: 'w1', kind: 'terminal', title: 'Main terminal', sort: 1,
    payload: null, deletable: true, created_at: 0, updated_at: 0,
    ...overrides,
  };
}

/** Every card the case names — the listed cards and every task's worker card. A case about an unopenable worker passes its own `openableCards`. */
export function openableCardsOf(cards: readonly CardWire[], tasks: readonly ReportTaskRow[]): ReadonlySet<string> {
  return new Set([
    ...cards.map((entry) => entry.id),
    ...tasks.flatMap((task) => {
      const workerCardId = task.execution === undefined ? task.workerCardId : task.execution.workerCardId;
      return workerCardId === null ? [] : [workerCardId];
    }),
  ]);
}

/* Only a holder for the `panel` prop; a test that passes `panel` explicitly opts out and gets the fixed value it asked for. */
function PanelHost({ props }: { props: TrackPageProps }) {
  const [panel, setPanel] = useState<Panel | null>(props.panel ?? null);
  return (
    <TrackPage
      {...props}
      panel={panel}
      onOpenPanel={(next) => { setPanel(next); props.onOpenPanel?.(next); }}
      onClosePanel={() => { setPanel(null); props.onClosePanel?.(); }}
    />
  );
}

export function renderPage(overrides: Partial<TrackPageProps> = {}): RenderResult {
  const cards = overrides.cards ?? [];
  const tasks = overrides.tasks ?? [];
  const props: TrackPageProps = {
    mobilePanelObscured: false,
    track: track(),
    cards,
    tasks,
    openableCards: openableCardsOf(cards, tasks),
    canReopenTrack: false,
    canCloseTrack: false,
    adminGrantedAt: null,
    onRenameTrack: vi.fn(),
    onReopenTrack: vi.fn(),
    onCloseTrack: vi.fn(),
    onDeleteTrack: vi.fn(),
    onTrackDeleted: vi.fn(),
    ...overrides,
  };
  return render(<PanelHost props={props} />);
}

/** Capture real lazy menu contents after opening them; never synthesize projection fields or actions. */
export async function captureContextProjection(root: Element): Promise<Element> {
  const { default: userEvent } = await import('@testing-library/user-event');
  const { waitFor } = await import('@testing-library/react');
  const snapshot = root.cloneNode(true) as Element;
  const originals = [...root.querySelectorAll('[data-nc-row]')];
  const copies = [...snapshot.querySelectorAll('[data-nc-row]')];
  for (const [index, row] of originals.entries()) {
    const trigger = row.querySelector<HTMLButtonElement>('button[aria-haspopup="menu"]');
    if (trigger === null) continue;
    await userEvent.click(trigger);
    const menu = await waitFor(() => {
      const element = document.getElementById(trigger.getAttribute('aria-controls') ?? '');
      if (element === null) throw new Error('The row menu did not mount');
      return element;
    });
    // A portal may already occur elsewhere in the initial snapshot. Re-home only this
    // trigger's aria-controls target; unowned action nodes remain for the oracle to reject.
    for (const candidate of snapshot.querySelectorAll('[role="menu"]')) {
      if (candidate.id === menu.id) candidate.remove();
    }
    copies[index].append(menu.cloneNode(true));
    await userEvent.keyboard('{Escape}');
  }
  return snapshot;
}
