import { withScrollRestoration } from './follow-scroll.ts';

/** Restore a control after DOM changes without declaring reading navigation.
 * The host's existing restoration fence also protects tail-following intent. */
export function restoreDrawerFocus(element: HTMLElement): void {
  const pane = element.closest<HTMLElement>('[data-nc-drawer-scroll]');
  withScrollRestoration(pane === null ? [] : [pane], () => element.focus({ preventScroll: true }));
}
