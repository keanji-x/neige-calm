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

  /*
   * Unfold whatever the target is inside of, before measuring where it is.
   *
   * A `task` block is a `<details>` (see `features/report/task`), so from the
   * moment it could be folded, every arrival path could land on a row with the
   * answer hidden under it — the outline, a `neige://` link from another
   * report, a backlink, and the panel's task inventory, all four. Scrolling to
   * a closed disclosure and flashing it is a worse failure than not moving:
   * the reader is told "it is here" and shown a title.
   *
   * Both directions are needed and they are different cases. The *ancestor*
   * walk is for an anchor nested inside a fold; `element` itself is the block
   * wrapper (`document/public.tsx` puts the id on the `div` around the block),
   * so the `<details>` is a descendant, not a parent. Neither one alone covers
   * the other.
   *
   * Opening is deliberate and one-way — this never re-folds anything. Arriving
   * is a request to read; leaving is not a request to put it back, and a block
   * that snapped shut when the reader scrolled past would be the app taking
   * back something it was asked for.
   */
  for (const details of element.querySelectorAll('details')) details.open = true;
  for (
    let ancestor = element.closest('details');
    ancestor !== null;
    ancestor = ancestor.parentElement?.closest('details') ?? null
  ) {
    ancestor.open = true;
  }

  element.scrollIntoView({ block: 'start', behavior: 'auto' });
  // Re-arming needs the attribute to actually leave the DOM between two
  // arrivals at the same anchor, or the transition has nothing to run from.
  element.removeAttribute(ARRIVAL_ATTRIBUTE);
  requestAnimationFrame(() => { element.setAttribute(ARRIVAL_ATTRIBUTE, ''); });
}
