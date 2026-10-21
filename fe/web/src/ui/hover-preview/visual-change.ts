const VISUAL_CHANGE = 'nc-preview-ancestor-visual-change';

/** Synchronous geometry invalidation, never an admission or open request.
 * Owners call after painting an ancestor. The DOM document is the transport;
 * there is no registry, retained notice, observer or animation clock here. */
export function notifyAncestorVisualChange(ancestor: HTMLElement): void {
  ancestor.ownerDocument.dispatchEvent(new CustomEvent(VISUAL_CHANGE, { detail: ancestor }));
}

/** Owner-internal subscription; the consumer validates current dependencies
 * and admission on every notice, including notices captured before revocation. */
export function subscribeAncestorVisualChange(document: Document, receive: (ancestor: HTMLElement) => void): () => void {
  const listener = (event: Event) => {
    const ancestor: unknown = (event as CustomEvent<unknown>).detail;
    if (ancestor instanceof HTMLElement && ancestor.ownerDocument === document) receive(ancestor);
  };
  document.addEventListener(VISUAL_CHANGE, listener);
  return () => { document.removeEventListener(VISUAL_CHANGE, listener); };
}
