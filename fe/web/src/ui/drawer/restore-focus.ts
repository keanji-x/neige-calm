import { useCallback, useEffect, useLayoutEffect, useRef, type RefObject } from 'react';
import { withScrollRestoration } from './follow-scroll.ts';

/** Restore a control after DOM changes without declaring reading navigation.
 * The host's existing restoration fence also protects tail-following intent. */
export function restoreDrawerFocus(element: HTMLElement): void {
  const pane = element.closest<HTMLElement>('[data-nc-drawer-scroll]');
  withScrollRestoration(pane === null ? [] : [pane], () => element.focus({ preventScroll: true }));
}

/** Switcher exposes its native dialog, but no finalFocusRef. Observe native exit
 * completion rather than focusing while a modal/animating shell is still open.
 * A presentation change while logically open must preserve the editor instead. */
export function useNativeDialogFinalFocus(dialog: HTMLDialogElement | null, open: boolean, finalFocus: RefObject<HTMLElement | null>): void {
  const presented = useRef(false);
  useLayoutEffect(() => {
    if (dialog === null) return;
    const sync = () => {
      if (dialog.open) { presented.current = true; return; }
      if (!presented.current) return;
      presented.current = false;
      if (!open && finalFocus.current?.isConnected) restoreDrawerFocus(finalFocus.current);
    };
    const observer = new MutationObserver(sync);
    observer.observe(dialog, { attributes: true, attributeFilter: ['open'] });
    sync();
    return () => observer.disconnect();
  }, [dialog, open, finalFocus]);
}

/** Retain the focused editor across moves of one stable portal container.
 * Range clones are live: removing the container rewrites their boundaries.
 * Keep node/offset values instead, and restore after native presentation settles. */
export function usePortalFocusRetention(host: HTMLElement, dialog: HTMLDialogElement | null, native: boolean, open: boolean, attached: boolean): () => void {
  const retained = useRef<Readonly<{
    element: HTMLElement;
    selection: Readonly<{ anchor: Node; anchorOffset: number; focus: Node; focusOffset: number }> | null;
  }> | null>(null);
  const retain = useCallback(() => {
    const element = document.activeElement;
    if (!(element instanceof HTMLElement) || !host.contains(element)) return;
    const selection = document.getSelection();
    retained.current = { element, selection: selection?.anchorNode != null && selection.focusNode !== null
      && host.contains(selection.anchorNode) && host.contains(selection.focusNode)
      ? { anchor: selection.anchorNode, anchorOffset: selection.anchorOffset, focus: selection.focusNode, focusOffset: selection.focusOffset } : null };
  }, [host]);
  useEffect(() => {
    // A close ends this move's snapshot; reopening must capture a new caret.
    if (!open) { retained.current = null; return; }
    if (!attached) return;
    const restore = () => {
      const snapshot = retained.current;
      if (snapshot === null) return;
      if (!host.contains(snapshot.element)) { retained.current = null; return; }
      // Leaving a native host must wait for close(), too: its exit can reset
      // selection even after focus has already moved to the desktop destination.
      if (!host.isConnected || (dialog !== null && dialog.open !== native)
        || snapshot.element.closest('[inert], [aria-hidden="true"]') !== null) return;
      // Consume before focus: synchronous focus handlers may change the DOM.
      retained.current = null;
      restoreDrawerFocus(snapshot.element);
      if (document.activeElement !== snapshot.element) return;
      const selection = snapshot.selection;
      const validBoundary = (node: Node, offset: number) => node.isConnected && host.contains(node)
        && offset >= 0 && offset <= (node instanceof CharacterData ? node.length : node.childNodes.length);
      if (selection !== null && validBoundary(selection.anchor, selection.anchorOffset) && validBoundary(selection.focus, selection.focusOffset)) {
        document.getSelection()?.setBaseAndExtent(selection.anchor, selection.anchorOffset, selection.focus, selection.focusOffset);
      }
    };
    restore();
    if (dialog === null) return;
    const observer = new MutationObserver(restore);
    observer.observe(dialog, { attributes: true, attributeFilter: ['open'] });
    return () => observer.disconnect();
  }, [attached, dialog, host, native, open]);
  return retain;
}
