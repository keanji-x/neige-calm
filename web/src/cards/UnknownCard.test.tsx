// Unit tests for the unknown-card fallback rendering.
//
// Two surfaces share responsibility for this story and they need to stay
// aligned:
//
//   1. `adaptKernelCard` (cards/registry.ts) returns null when no registry
//      entry's `fromKernel` claims a kernel card. Router code then builds a
//      `WaveCardSlot` with `kind: 'unknown'` instead of `kind: 'card'`.
//   2. `UnknownCard` (this folder) is the placeholder UI for those slots.
//
// We test both seams:
//   - The adapter null contract: a kind that isn't registered yields null,
//     so the upstream switch picks the `unknown` slot.
//   - The component renders, surfaces the kernel's `kind` string, and is
//     marked draggable (header carries `card-drag-handle` so react-grid-
//     layout's drag handle scoping doesn't break for unknown panels).

import { describe, it, expect } from 'vitest';
import { render, screen } from '@testing-library/react';
import { UnknownCard, UNKNOWN_CARD_SIZE } from './UnknownCard';
import { adaptKernelCard } from './registry';
import type { KernelCard } from '../api/wire';

describe('UnknownCard component', () => {
  it('renders the kernel kind string', () => {
    render(<UnknownCard kernelKind="frobnicator" />);
    // The kind appears verbatim inside a <code> block so devs can spot
    // schema drift at a glance.
    expect(screen.getByText('frobnicator')).toBeInTheDocument();
    expect(screen.getByText(/Unknown card/i)).toBeInTheDocument();
  });

  it('marks its header with `card-drag-handle` so RGL drag scope still works', () => {
    const { container } = render(<UnknownCard kernelKind="x" />);
    const handle = container.querySelector('.card-drag-handle');
    expect(handle).not.toBeNull();
    // The header is the drag handle; its text identifies the slot to the
    // user. The drag-handle class is what couples it to react-grid-layout.
    expect(handle?.textContent).toMatch(/Unknown card/i);
  });

  it('exports a sane default size for the wave grid layout', () => {
    // Pinned to keep the wave grid from collapsing if the unknown slot is
    // the only thing in a wave — w/h must be non-zero and respect minW/H.
    expect(UNKNOWN_CARD_SIZE.w).toBeGreaterThanOrEqual(UNKNOWN_CARD_SIZE.minW);
    expect(UNKNOWN_CARD_SIZE.h).toBeGreaterThanOrEqual(UNKNOWN_CARD_SIZE.minH);
    expect(UNKNOWN_CARD_SIZE.minW).toBeGreaterThan(0);
    expect(UNKNOWN_CARD_SIZE.minH).toBeGreaterThan(0);
  });
});

describe('adaptKernelCard fallback contract', () => {
  it('returns null when no registry entry claims the kernel card kind', () => {
    // We intentionally do not call `registerBuiltins()` here so the registry
    // is empty (or near-empty if another test already populated it). An
    // unregistered, made-up kind is the canonical "unknown" path.
    const card: KernelCard = {
      id: 'card_unk',
      wave_id: 'wave_1',
      kind: 'definitely-not-a-real-kind',
      sort: 0,
      payload: null,
      created_at: 1,
      updated_at: 2,
    };
    // The registry iterates entries and returns the first non-null
    // `fromKernel`. With no entry claiming this kind, the result is null —
    // which is exactly the signal router code uses to emit a `kind:
    // 'unknown'` slot for the UnknownCard component above.
    expect(adaptKernelCard(card)).toBeNull();
  });
});
