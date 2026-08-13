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
