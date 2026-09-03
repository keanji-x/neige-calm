// The mobile dock's four destinations, as data (#1191 §3.3).
//
// They were four hand-written `<button>`s whose only real differences were an
// icon, a label, what the press does, and whether the press opens a shell sheet
// — with the selection rule spread across four `aria-current` expressions that
// had to be read together to see that exactly one is ever true. Here the rule is
// one pure function, and the difference that actually matters to a11y —
// `aria-controls`/`aria-expanded` — is a single optional field.

import { pathFor } from '../router/navigation.ts';

export type MobileSection = 'pages' | 'areas';
export type DockKey = 'pages' | 'today' | 'areas' | 'me';

export type DockItem = Readonly<{
  key: DockKey;
  label: string;
  icon: 'viewColumns' | 'calendar' | 'menu' | 'wrench';
  /**
   * The sheet this item opens, if it opens one.
   *
   * This is what `aria-controls` / `aria-expanded` are driven from, and its
   * absence on Today and Me is **correct, not an omission** (#1191 §3.3): those
   * two navigate to a route and control no expandable region, so giving them
   * `aria-controls="mobile-workspace-navigation"` would claim they operate a
   * region they never touch. An earlier round proposed "fixing" that by adding
   * the attributes everywhere; the direction is the other way.
   */
  opensSection?: MobileSection;
}>;

/**
 * Frozen all the way down: `architecture/no-module-runtime-state` treats a
 * frozen array of unfrozen objects as module runtime state, because it is —
 * anything holding a reference could still edit a row.
 */
export const DOCK_ITEMS: readonly DockItem[] = Object.freeze([
  Object.freeze({ key: 'pages', label: 'Pages', icon: 'viewColumns', opensSection: 'pages' } as const),
  Object.freeze({ key: 'today', label: 'Today', icon: 'calendar' } as const),
  Object.freeze({ key: 'areas', label: 'Areas', icon: 'menu', opensSection: 'areas' } as const),
  Object.freeze({ key: 'me', label: 'Me', icon: 'wrench' } as const),
]);

/**
 * Which single dock item is current, given the open sheet and the path.
 *
 * Pages is the fallback rather than a fifth "nothing selected" state, which is
 * the behaviour the four inline `aria-current` expressions added up to: on a
 * Track route with no sheet open, the reader is inside the Pages index. A sheet
 * always wins over the route underneath it, because the sheet is
 * what they are looking at.
 */
export function dockSelection(section: MobileSection | null, path: string): DockKey {
  if (section !== null) return section;
  if (path === pathFor({ name: 'today' })) return 'today';
  // The route table is `pathFor`'s, not a literal repeated here; settings is
  // matched as a prefix so a future sub-page keeps the tab lit.
  const settings = pathFor({ name: 'settings' });
  if (path === settings || path.startsWith(`${settings}/`)) return 'me';
  return 'pages';
}
