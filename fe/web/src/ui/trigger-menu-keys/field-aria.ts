import { useLayoutEffect, type RefObject } from 'react';

/**
 * Keeps `aria-multiline` off a composer field under `root` while that field is a `combobox`, which
 * is what a trigger menu makes it; the attribute is not allowed on that role (axe aria-allowed-attr).
 * Astryx 0.1.3 hard-codes it on the editable (`ChatComposerInput.tsx:635`) and exposes no prop for
 * it; 0.6.0 emits it only for the trigger-less `textbox`. Delete this with the bump in #1891.
 *
 * Watched, not applied once: React sets the attribute again whenever it recreates the field, and the
 * role flips to `combobox` whenever a trigger is added after mount.
 */
export function useTriggerFieldAria(root: RefObject<HTMLElement | null>): void {
  useLayoutEffect(() => {
    const host = root.current;
    if (host === null) return undefined;
    const sync = () => {
      for (const field of host.querySelectorAll('[contenteditable][role="combobox"][aria-multiline]')) {
        field.removeAttribute('aria-multiline');
      }
    };
    sync();
    const observer = new MutationObserver(sync);
    observer.observe(host, { subtree: true, childList: true, attributes: true, attributeFilter: ['role', 'aria-multiline'] });
    return () => { observer.disconnect(); };
  }, [root]);
}
