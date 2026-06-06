import { useEffect, type RefObject } from 'react';
import { resolveCardById } from './resolver';

const CARD_SHELL_SELECTOR = '[data-card-id]';

function cardShellFor(target: EventTarget | null): HTMLElement | null {
  return target instanceof Element
    ? target.closest<HTMLElement>(CARD_SHELL_SELECTOR)
    : null;
}

function cardShellsIn(node: Node): HTMLElement[] {
  if (!(node instanceof HTMLElement)) return [];
  const shells: HTMLElement[] = [];
  if (node.matches(CARD_SHELL_SELECTOR)) shells.push(node);
  shells.push(...node.querySelectorAll<HTMLElement>(CARD_SHELL_SELECTOR));
  return shells;
}

export function useCardVisibilityFocus(
  scrollRootRef: RefObject<HTMLElement | null>,
): void {
  useEffect(() => {
    const scrollRoot = scrollRootRef.current;
    if (!scrollRoot) return;

    const handleFocusIn = (event: FocusEvent) => {
      const cardId = cardShellFor(event.target)?.dataset.cardId;
      if (!cardId) return;
      resolveCardById(cardId)?.writer.setFocused(true);
    };
    const handleFocusOut = (event: FocusEvent) => {
      const fromShell = cardShellFor(event.target);
      const toShell = cardShellFor(event.relatedTarget);
      const cardId = fromShell?.dataset.cardId;
      if (!cardId || toShell === fromShell) return;
      resolveCardById(cardId)?.writer.setFocused(false);
    };

    scrollRoot.addEventListener('focusin', handleFocusIn);
    scrollRoot.addEventListener('focusout', handleFocusOut);

    let intersectionObserver: IntersectionObserver | null = null;
    let mutationObserver: MutationObserver | null = null;
    const observed = new Set<HTMLElement>();

    const observeShell = (shell: HTMLElement) => {
      if (!shell.dataset.cardId || observed.has(shell)) return;
      intersectionObserver?.observe(shell);
      observed.add(shell);
    };
    const unobserveShell = (shell: HTMLElement) => {
      if (!observed.delete(shell)) return;
      intersectionObserver?.unobserve(shell);
    };

    if (typeof IntersectionObserver === 'function') {
      intersectionObserver = new IntersectionObserver(
        (entries) => {
          for (const entry of entries) {
            const cardId = (entry.target as HTMLElement).dataset.cardId;
            if (!cardId) continue;
            resolveCardById(cardId)?.writer.setVisible(entry.isIntersecting);
          }
        },
        { root: scrollRoot, threshold: 0 },
      );

      for (const shell of scrollRoot.querySelectorAll<HTMLElement>(
        CARD_SHELL_SELECTOR,
      )) {
        observeShell(shell);
      }

      if (typeof MutationObserver === 'function') {
        mutationObserver = new MutationObserver((mutations) => {
          for (const mutation of mutations) {
            for (const node of mutation.addedNodes) {
              for (const shell of cardShellsIn(node)) observeShell(shell);
            }
            for (const node of mutation.removedNodes) {
              for (const shell of cardShellsIn(node)) unobserveShell(shell);
            }
          }
        });
        mutationObserver.observe(scrollRoot, { childList: true, subtree: true });
      }
    }

    return () => {
      scrollRoot.removeEventListener('focusin', handleFocusIn);
      scrollRoot.removeEventListener('focusout', handleFocusOut);
      mutationObserver?.disconnect();
      intersectionObserver?.disconnect();
      observed.clear();
    };
  }, [scrollRootRef]);
}
