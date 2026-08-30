// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ARRIVAL_ATTRIBUTE, revealReportAnchor } from './public.ts';

beforeEach(() => {
  document.body.innerHTML = '<div id="b-1">one</div><div id="b-2">two</div>';
  // jsdom implements neither, and both are called on every arrival.
  Element.prototype.scrollIntoView = vi.fn();
  vi.stubGlobal('requestAnimationFrame', (callback: FrameRequestCallback) => {
    callback(0);
    return 0;
  });
});

afterEach(() => { vi.unstubAllGlobals(); });

describe('revealReportAnchor', () => {
  it('scrolls the block into view and marks it for the one-shot highlight', () => {
    const scrollIntoView = vi.spyOn(
      document.getElementById('b-2') as HTMLElement, 'scrollIntoView',
    );
    revealReportAnchor('b-2');
    const target = document.getElementById('b-2');
    expect(scrollIntoView).toHaveBeenCalled();
    expect(target?.hasAttribute(ARRIVAL_ATTRIBUTE)).toBe(true);
    expect(document.getElementById('b-1')?.hasAttribute(ARRIVAL_ATTRIBUTE)).toBe(false);
  });

  // A stale backlink, or a block the agent has since rewritten. An error about
  // a document the reader can already see would be noise.
  it('does nothing at all for an anchor that is not on the page', () => {
    expect(() => revealReportAnchor('b-gone')).not.toThrow();
  });

  /*
   * ── Landing on a folded block ────────────────────────────────────────────
   *
   * A `task` block is a `<details>` (`features/report/task`), so from the
   * moment it could be folded, all four arrival paths — the outline, a
   * `neige://` link from another report, a backlink, and the panel's task
   * inventory — could scroll to a row with the answer hidden under it and flash
   * the title. That is worse than not moving: the reader is told "it is here"
   * and shown nothing.
   *
   * Both directions are asserted because they are different cases and neither
   * covers the other. `document/public.tsx` puts the block id on the `div`
   * *around* the block, so the `<details>` is a descendant of the target; an
   * anchor nested inside a fold is the other way round.
   */
  it('unfolds a disclosure inside the block it lands on', () => {
    document.body.innerHTML = '<div id="b-3"><details><summary>Task</summary>detail</details></div>';
    revealReportAnchor('b-3');
    expect(document.querySelector('details')?.open).toBe(true);
  });

  it('unfolds the disclosures the anchor is nested inside', () => {
    document.body.innerHTML =
      '<details id="outer"><summary>Outer</summary>'
      + '<details id="inner"><summary>Inner</summary><span id="b-4">deep</span></details>'
      + '</details>';
    revealReportAnchor('b-4');
    expect(document.getElementById('inner')).toHaveProperty('open', true);
    expect(document.getElementById('outer')).toHaveProperty('open', true);
  });

  /* One way only. Arriving is a request to read; scrolling away is not a
     request to put it back, and a block that re-folded itself would be the app
     taking back what it was asked for. */
  it('leaves an already-open disclosure open, and never folds one', () => {
    document.body.innerHTML = '<div id="b-5"><details open><summary>Task</summary>detail</details></div>';
    revealReportAnchor('b-5');
    expect(document.querySelector('details')?.open).toBe(true);
  });

  // Arriving twice at the same anchor has to flash twice, and a transition that
  // is already at its end state has nothing to run from.
  it('re-arms the marker on a second arrival at the same anchor', () => {
    revealReportAnchor('b-1');
    const target = document.getElementById('b-1');
    const removeAttribute = vi.spyOn(target as HTMLElement, 'removeAttribute');
    revealReportAnchor('b-1');
    expect(removeAttribute).toHaveBeenCalledWith(ARRIVAL_ATTRIBUTE);
    expect(target?.hasAttribute(ARRIVAL_ATTRIBUTE)).toBe(true);
  });
});
