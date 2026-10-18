import { useCallback, useEffectEvent, useLayoutEffect, useRef, type RefObject } from 'react';

// Geometry hosting and interaction ownership are independent. This is a bounded
// DOM-platform scope, not a vendor lifecycle adapter. Nested preview ownership
// is already carried by the typed parent port; do not reinterpret its temporary
// preparation inertness as permanent host revocation.
function chain(source: HTMLElement | null): HTMLElement[] {
  const nodes: HTMLElement[] = [];
  for (let node = source; node !== null; node = node.parentElement) {
    if (node !== source && node.hasAttribute('data-nc-preview-owner')) break;
    nodes.push(node);
  }
  return nodes;
}
function permitted(nodes: readonly HTMLElement[]) {
  if (nodes.length === 0 || !nodes[0].isConnected) return false;
  return nodes.every(node => {
    const style = getComputedStyle(node);
    return !node.inert && !node.hidden && (!(node instanceof HTMLDialogElement) || node.open)
      && style.display !== 'none' && style.contentVisibility !== 'hidden';
  }) && !['hidden', 'collapse'].includes(getComputedStyle(nodes[0]).visibility);
}

/** Every authority consumer calls read(), including before observer delivery.
 * The observer clears dormant requests/DOM permissions before the next input
 * task. Only the source's ancestor edges and permission-bearing attributes are
 * observed, never document subtrees, descendants or a recurring frame clock.
 */
export function useInteractionScope(source: RefObject<HTMLElement | null>, revoke: () => void) {
  const onRevoke = useEffectEvent(revoke);
  const scope = useRef<{ read: () => boolean; dispose: () => void } | null>(null);
  const read = useCallback(() => scope.current?.read() ?? permitted(chain(source.current)), [source]);
  useLayoutEffect(() => {
    let nodes = chain(source.current);
    let allowed = permitted(nodes);
    let connected = nodes[0]?.isConnected ? nodes : [];
    let watched = nodes;
    let live = true;
    const observer = new MutationObserver(records => { check(records); });
    const bind = () => {
      observer.disconnect();
      for (const node of watched) observer.observe(node, {
        childList: true, attributes: true, attributeOldValue: true,
        attributeFilter: ['inert', 'hidden', 'open', 'style', 'class'],
      });
      if (source.current !== null) observer.observe(source.current.ownerDocument, { childList: true });
    };
    const check = (delivered: MutationRecord[] = []) => {
      if (!live) return false;
      // Drain synchronously too: a disable/re-enable or detach/reinsert in one
      // task cannot lend a previous generation authority again.
      const records = [...delivered, ...observer.takeRecords()];
      const next = chain(source.current);
      const replaced = nodes.length !== next.length || nodes.some((node, index) => node !== next[index]);
      const interrupted = records.some(record => record.type === 'childList'
        ? record.target === source.current && record.removedNodes.length > 0 || Array.from(record.removedNodes).some(removed => nodes.some(node => removed === node))
        : (record.attributeName === 'inert' || record.attributeName === 'hidden') && record.oldValue !== null
          || record.attributeName === 'open' && record.oldValue === null);
      const nextAllowed = permitted(next);
      const lost = replaced || interrupted || allowed && !nextAllowed;
      nodes = next; allowed = nextAllowed;
      if (replaced) {
        // Keep the former ancestor edges during detachment so reinsertion is
        // observed too. A connected replacement releases the obsolete chain.
        if (next[0]?.isConnected) connected = next;
        watched = next[0]?.isConnected ? next : Array.from(new Set([...connected, ...next]));
        bind();
      }
      if (lost) onRevoke();
      // Revocation invalidated every old publication/generation. A new explicit
      // intent may now use the current scope; stale timers check generation AFTER
      // this read, while admission re-reads its publication after it.
      return allowed;
    };
    bind();
    scope.current = { read: check, dispose: () => { live = false; observer.disconnect(); } };
    if (!allowed) onRevoke();
    return () => { scope.current?.dispose(); scope.current = null; };
  }, [source]);
  useLayoutEffect(() => { read(); });
  return read;
}
