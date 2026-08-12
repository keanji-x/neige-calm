// Landing on a block.
//
// Three paths arrive at the same place — the outline (§6.16), a `neige://`
// link inside a report, and a backlink from another wave — so the scroll and
// the one-shot highlight live here once rather than three times.
//
// §2.6 allows exactly one non-overlay entrance effect in the app, and this is
// it: the element was already on the page, and the flash is the document
// pointing at it. It is a *one-shot* colour change, not a loop, and it is
// carried by a data attribute the CSS transitions off, so nothing here
// animates layout.

/** Marks the arrival target for one paint; `document.module.css` fades it out. */
export const ARRIVAL_ATTRIBUTE = 'data-nc-arrived';

/**
 * Scroll `anchorId` into view and flash it.
 *
 * Fail-soft by construction: an anchor that is not on the page (a stale
 * backlink, a block the agent has since rewritten) does nothing at all. A
 * "could not find that section" error would be noise about a document the
 * reader can already see.
 */
export function revealReportAnchor(anchorId: string, root: Document = document): void {
  // `getElementById`, never a selector: a block id comes from the kernel and a
  // heading id is derived from it, but neither is a thing to concatenate into
  // a query — `#` plus arbitrary text is a selector-injection shape, and the
  // id lookup has no syntax to inject into.
  const element = root.getElementById(anchorId);
  if (element === null) return;

  element.scrollIntoView({ block: 'start', behavior: 'auto' });
  // Re-arming needs the attribute to actually leave the DOM between two
  // arrivals at the same anchor, or the transition has nothing to run from.
  element.removeAttribute(ARRIVAL_ATTRIBUTE);
  requestAnimationFrame(() => { element.setAttribute(ARRIVAL_ATTRIBUTE, ''); });
}
