// The conversation drawer's dragged width. It is a fact about the main region, not
// about the card: `--conversation-span` on `.main` also sizes the covered panel track,
// the report measure and the rail preview, so the shell owns the value and `ui/drawer`
// only reports where its edge went. The stylesheet applies it while a resizable drawer
// is open (`data-nc-drawer-resizable`), so a source card on its own keeps the default.
// The routes reach the contract through `useConversationDrawerResize` in `./public.tsx`.

import { useLayoutEffect, useMemo, useRef, type CSSProperties } from 'react';

import { useState } from '../../ui/state/public.ts';
import { useSpringTarget } from '../../ui/motion/target.ts';
import { useCompactViewport } from '../../ui/viewport/public.ts';
import type { DrawerResize } from '../../ui/drawer/public.tsx';
import type { UiPreferences } from '../providers/ui-preferences.tsx';

const WIDTH_PROPERTY = '--nc-drawer-width';

export function useDrawerWidthHost(preferences: UiPreferences) {
  const mainRef = useRef<HTMLElement | null>(null);
  const [open, setOpen] = useState(false);
  const compact = useCompactViewport();
  useLayoutEffect(() => {
    const main = mainRef.current;
    if (main === null) return;
    const read = () => { setOpen(main.querySelector('[data-nc-drawer-open]') !== null); };
    const observer = new MutationObserver(records => {
      if (records.some(record => record.type === 'attributes'
        || [...record.addedNodes, ...record.removedNodes].some(node => node instanceof Element
          && (node.matches('[data-nc-drawer]') || node.querySelector('[data-nc-drawer]') !== null)))) read();
    });
    observer.observe(main, { childList: true, subtree: true, attributes: true, attributeFilter: ['data-nc-drawer-open'] });
    read();
    return () => { observer.disconnect(); };
  }, []);
  useSpringTarget(mainRef, null, open ? 1 : 0, 0, true, !compact, () => {},
    value => ({ '--nc-panel-open': value }));
  const width = preferences.drawerWidth();
  /* A drag writes the property straight onto `.main` once a frame, so the whole route is not re-rendered at pointer rate. React only rewrites a style property whose value changed, so the stored value never fights a drag in progress; a commit writes both. */
  const resize = useMemo<DrawerResize>(() => {
    const preview = (rem: number | null) => {
      const main = mainRef.current;
      if (main === null) return;
      if (rem === null) main.style.removeProperty(WIDTH_PROPERTY);
      else main.style.setProperty(WIDTH_PROPERTY, `${rem}rem`);
    };
    return {
      onPreview: preview,
      onCommit: (rem) => { preview(rem); preferences.setDrawerWidth(rem); },
    };
  }, [preferences]);
  const style = width === null ? undefined : { [WIDTH_PROPERTY]: `${width}rem` } as CSSProperties;
  return { mainRef, style, resize };
}
