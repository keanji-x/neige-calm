// Shared fixtures for the WavePage behaviour and contract suites.
//
// `Wave` carries the plugin activity fields, so the factory spreads
// `NEUTRAL_ACTIVITY` — "no plugin has posted anything" is a value, not a hole.

import { render, type RenderResult } from '@testing-library/react';
import { vi } from 'vitest';

import type { Cove } from '../../../../../core/domain/cove.ts';
import { NEUTRAL_ACTIVITY, type CardWire, type Wave } from '../../../../../core/domain/wave.ts';
import { WavePage, type WavePageProps } from './public.tsx';

export function cove(overrides: Partial<Cove> = {}): Cove {
  return { id: 'c1', name: 'Work', color: '#5B8DEF', sort: 1, kind: 'user', createdAt: 0, updatedAt: 0, ...overrides };
}

export function wave(overrides: Partial<Wave> = {}): Wave {
  return {
    id: 'w1', coveId: 'c1', title: 'Alpha', sort: 1, lifecycle: 'working', cwd: '/tmp/alpha',
    archivedAt: null, pinnedAt: null, terminalAt: null, createdAt: 0, updatedAt: 0,
    ...NEUTRAL_ACTIVITY,
    ...overrides,
  };
}

export function card(overrides: Partial<CardWire> = {}): CardWire {
  return {
    id: 'card-1', wave_id: 'w1', kind: 'terminal', title: 'Main terminal', sort: 1,
    payload: null, deletable: true, created_at: 0, updated_at: 0,
    ...overrides,
  };
}

export function renderPage(overrides: Partial<WavePageProps> = {}): RenderResult {
  const props: WavePageProps = {
    wave: wave(),
    cove: cove(),
    cards: [],
    onOpenCove: vi.fn(),
    onOpenToday: vi.fn(),
    onRenameWave: vi.fn(),
    onDeleteWave: vi.fn(),
    ...overrides,
  };
  return render(<WavePage {...props} />);
}
