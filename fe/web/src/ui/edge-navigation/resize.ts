/** Observe layout where the browser supplies it; DOM-only tests have no geometry. */
export function observeResize(element: Element, onResize: () => void): () => void {
  if (typeof ResizeObserver === 'undefined') return () => {};
  const observer = new ResizeObserver(onResize);
  observer.observe(element);
  return () => { observer.disconnect(); };
}
