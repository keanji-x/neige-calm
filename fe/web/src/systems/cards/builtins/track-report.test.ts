import { describe, expect, it } from 'vitest';

import type { CardEntry } from '../registry.js';
import { createCardRegistry } from '../registry.js';
import { registerAvailableBuiltinCards } from './register.js';
import type { TrackReportCard } from './track-report.js';
import { TRACK_REPORT_CARD_ENTRY } from './track-report.js';

describe('track-report card entry', () => {
  it('[INV-CARD-201] is headless and kernel-minted only, with no add-panel entry point', () => {
    expect((TRACK_REPORT_CARD_ENTRY.component as unknown as (props: unknown) => unknown)({})).toBeNull();
    expect(TRACK_REPORT_CARD_ENTRY.create).toEqual({ mode: 'kernel-minted-only' });
    expect(TRACK_REPORT_CARD_ENTRY.defaultSize).toEqual({ w: 1, h: 1, minW: 1, minH: 1 });
  });

  it('[INV-CARD-201] takes no claim, so it stays on the insertion-ordered fallback scan', () => {
    expect((TRACK_REPORT_CARD_ENTRY as CardEntry<TrackReportCard>).claim).toBeUndefined();
  });

  it('[INV-CARD-201] resolves the track-report kernel kind and nothing else', () => {
    expect(TRACK_REPORT_CARD_ENTRY.fromKernel?.({ id: 'r1', kind: 'track-report', payload: null }))
      .toEqual({ type: 'track-report', id: 'r1' });
    expect(TRACK_REPORT_CARD_ENTRY.fromKernel?.({ id: 'r1', kind: 'codex', payload: null })).toBeNull();
    expect(TRACK_REPORT_CARD_ENTRY.fromKernel?.({ id: 'r1', kind: 'report', payload: null })).toBeNull();
  });

  it('resolves through a really booted registry', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(registry.resolve({ id: 'r1', kind: 'track-report', payload: null })?.type).toBe('track-report');
  });
});
