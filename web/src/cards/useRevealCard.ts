// Bring one card into view, once, wherever the track is currently rendering
// cards.
//
// The caller is a report task block's "Open worker output" link, which lands on
// `/track/$trackId#<workerCardId>`. Both card views mount a scroll root and stamp
// `data-card-id` on every tile (`TrackGrid`, `TrackList`), so the reveal is the
// same operation in both — a second copy in each would be two places to forget
// the latch.
//
// **The latch is the point.** An earlier draft keyed the effect on
// `[revealCardId, cardKeys]`, which reads as "retry when the cards arrive" but
// actually means "re-fire whenever the card list changes at all": the hash
// stays in the URL for the life of the page, so every later card added or
// removed anywhere in the track yanked the viewport back to the linked card.
// Running on every render and latching on the id instead retries until the tile
// exists and then stops for good — the retry and the one-shot are the same
// mechanism, so neither can be fixed without the other.

import { useEffect, useRef, type RefObject } from 'react';

/**
 * Attribute that drives the arrival flash. Defined in `calm.css` beside
 * `.track-card`.
 *
 * An attribute rather than a class, because `react-grid-layout` computes the
 * grid item's `className` itself (`react-grid-item track-card react-draggable
 * …`) and rewrites it on every re-render — an imperatively added class was
 * silently wiped moments after being set, which jsdom could not show because
 * the RGL stub there does not manage className. React never removes a `data-*`
 * attribute it was not given as a prop, so this survives.
 */
const REVEAL_ATTR = 'data-nc-reveal';

export function useRevealCard(
  rootRef: RefObject<HTMLElement | null>,
  revealCardId: string | undefined,
): void {
  // The id this hook has already scrolled to. Not state: changing it must not
  // re-render, and the effect below reads it as a guard, never as a trigger.
  const revealedRef = useRef<string | undefined>(undefined);

  // Deliberately no dependency array. The tile may not exist on the render that
  // first sees the id (the grid is lazy-loaded, and the card list arrives from
  // a query), so this has to keep looking; `revealedRef` makes every render
  // after the first success a single string comparison.
  useEffect(() => {
    if (revealCardId === undefined) {
      // A new hash may name this same card again later, and it should scroll
      // again when it does — so forget rather than keep the old id latched.
      revealedRef.current = undefined;
      return;
    }
    if (revealedRef.current === revealCardId) return;
    const target = rootRef.current?.querySelector(
      `[data-card-id="${CSS.escape(revealCardId)}"]`,
    );
    if (!target) return;
    revealedRef.current = revealCardId;
    target.scrollIntoView({ block: 'nearest' });
    // Restart the animation rather than relying on the attribute being absent:
    // `revealedRef` is cleared and re-set across navigations, so it can still
    // be on the node from a previous visit.
    //
    // Synchronous reflow, not `requestAnimationFrame`. This effect runs on
    // every render by design, so a deferred add would be cancelled by the very
    // next render's cleanup before the frame ever arrived — the flash simply
    // never appeared. Reading `offsetWidth` between remove and add is the
    // standard restart idiom and needs no cleanup at all.
    target.removeAttribute(REVEAL_ATTR);
    void (target as HTMLElement).offsetWidth;
    target.setAttribute(REVEAL_ATTR, '');
  });
}
