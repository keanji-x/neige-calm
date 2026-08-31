// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { ARRIVAL_ATTRIBUTE, revealReportAnchor } from './public.ts';

beforeEach(() => {
  document.body.innerHTML = '<article data-nc-report=""><div id="b-1">one</div><div id="b-2">two</div></article>';
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
    document.body.innerHTML = '<article data-nc-report=""><div id="b-3"><details><summary>Task</summary>detail</details></div></article>';
    revealReportAnchor('b-3');
    expect(document.querySelector('details')?.open).toBe(true);
  });

  it('unfolds the disclosures the anchor is nested inside', () => {
    document.body.innerHTML =
      '<article data-nc-report=""><details id="outer"><summary>Outer</summary>'
      + '<details id="inner"><summary>Inner</summary><span id="b-4">deep</span></details>'
      + '</details></article>';
    revealReportAnchor('b-4');
    expect(document.getElementById('inner')).toHaveProperty('open', true);
    expect(document.getElementById('outer')).toHaveProperty('open', true);
  });

  /* One way only. Arriving is a request to read; scrolling away is not a
     request to put it back, and a block that re-folded itself would be the app
     taking back what it was asked for. */
  it('leaves an already-open disclosure open, and never folds one', () => {
    document.body.innerHTML = '<article data-nc-report=""><div id="b-5"><details open><summary>Task</summary>detail</details></div></article>';
    revealReportAnchor('b-5');
    expect(document.querySelector('details')?.open).toBe(true);
  });

  /*
   * **Order, not just outcome.** Scrolling to a closed disclosure and then
   * opening it puts the reader somewhere the layout has since moved: the
   * browser measures where to land while the content is still collapsed. The
   * unfold has to happen first, and "both things happened" is what an
   * end-state assertion checks. This one records the order.
   */
  it('unfolds before it measures where to scroll', () => {
    document.body.innerHTML =
      '<article data-nc-report=""><div id="b-6"><details><summary>Task</summary>detail</details></div></article>';
    const order: string[] = [];
    const details = document.querySelector('details')!;
    const target = document.getElementById('b-6')!;
    /* `open` is a property with a setter on the prototype; recording through it
       catches the write wherever in the function it happens. */
    const descriptor = Object.getOwnPropertyDescriptor(HTMLDetailsElement.prototype, 'open')!;
    Object.defineProperty(details, 'open', {
      configurable: true,
      get: (): boolean => descriptor.get!.call(details) as boolean,
      set: (value: boolean) => { order.push('open'); descriptor.set!.call(details, value); },
    });
    target.scrollIntoView = () => { order.push('scroll'); };

    revealReportAnchor('b-6');

    expect(order).toEqual(['open', 'scroll']);
  });

  /*
   * The anchor id comes from the route hash, so it is reader-supplied. `#root`
   * resolves to the application's own root element, and an unscoped descendant
   * walk would open every `<details>` on the page — every task in every report
   * on screen — because somebody pasted a URL with the wrong fragment.
   */
  it('does not unfold anything when the anchor is not inside a report', () => {
    document.body.innerHTML =
      '<div id="root"><article data-nc-report=""><details><summary>Task</summary>d</details></article></div>';
    revealReportAnchor('root');
    expect(document.querySelector('details')?.open).toBe(false);
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
