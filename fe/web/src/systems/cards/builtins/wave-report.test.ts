import { describe, expect, it } from 'vitest';

import type { CardEntry } from '../registry.js';
import { createCardRegistry } from '../registry.js';
import { registerAvailableBuiltinCards } from './register.js';
import type { WaveReportCard } from './wave-report.js';
import { WAVE_REPORT_CARD_ENTRY } from './wave-report.js';

describe('wave-report card entry', () => {
  it('[INV-CARD-201] is headless and kernel-minted only, with no add-panel entry point', () => {
    expect((WAVE_REPORT_CARD_ENTRY.component as unknown as (props: unknown) => unknown)({})).toBeNull();
    expect(WAVE_REPORT_CARD_ENTRY.create).toEqual({ mode: 'kernel-minted-only' });
    // `generic`, `atomic` and `catalog` are the three strategies that put a
    // card behind a user-facing add action; kernel-minted-only is the absence
    // of one, which is what "no addPanel entry" means at this layer, and the
    // deep equality above already excludes all three.
    expect(WAVE_REPORT_CARD_ENTRY.defaultSize).toEqual({ w: 1, h: 1, minW: 1, minH: 1 });
  });

  it('[INV-CARD-201] takes no claim, so it stays on the insertion-ordered fallback scan', () => {
    // Same standard as the sister headless entry `spec`: every builtin resolves
    // through `fromKernel` so one scan order decides every shared kernel kind.
    // Read through the interface: the entry literal is checked with `satisfies`
    // so registration can require `headless`, which means the constant's own
    // type only lists the members it declares. `claim` absent from that type is
    // the same fact this asserts at runtime.
    expect((WAVE_REPORT_CARD_ENTRY as CardEntry<WaveReportCard>).claim).toBeUndefined();
  });

  it('[INV-CARD-201] resolves the wave-report kernel kind and nothing else', () => {
    expect(WAVE_REPORT_CARD_ENTRY.fromKernel?.({ id: 'r1', kind: 'wave-report', payload: null }))
      .toEqual({ type: 'wave-report', id: 'r1' });
    expect(WAVE_REPORT_CARD_ENTRY.fromKernel?.({ id: 'r1', kind: 'codex', payload: null })).toBeNull();
    expect(WAVE_REPORT_CARD_ENTRY.fromKernel?.({ id: 'r1', kind: 'report', payload: null })).toBeNull();
  });

  it('resolves through a really booted registry', () => {
    const registry = createCardRegistry();
    registerAvailableBuiltinCards(registry);
    expect(registry.resolve({ id: 'r1', kind: 'wave-report', payload: null })?.type).toBe('wave-report');
  });
});
