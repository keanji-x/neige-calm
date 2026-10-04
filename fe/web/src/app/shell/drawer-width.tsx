// The conversation drawer's dragged width. It is a fact about the main region, not
// about the card: `--conversation-span` on `.main` also sizes the covered panel track,
// the report measure and the rail preview, so the shell owns the value and `ui/drawer`
// only reports where its edge went. The stylesheet applies it while a resizable drawer
// is open (`data-nc-drawer-resizable`), so a source card on its own keeps the default.
// The routes reach the contract through `useConversationDrawerResize` in `./public.tsx`.

import { useMemo, useRef, type CSSProperties } from 'react';

import type { DrawerResize } from '../../ui/drawer/public.tsx';
import type { UiPreferences } from '../providers/ui-preferences.tsx';

const WIDTH_PROPERTY = '--nc-drawer-width';

export function useDrawerWidthHost(preferences: UiPreferences) {
  const mainRef = useRef<HTMLElement | null>(null);
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
