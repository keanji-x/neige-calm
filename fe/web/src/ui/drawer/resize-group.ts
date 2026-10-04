import type { RefObject } from 'react';
import { readingPlaceIn, restoreReadingPlace, type ReadingPlace } from './reading-place.ts';

/** A shared width allocation and the scrolling panes affected by it. Owned by one host. */
export type PaneResizeGroup = Readonly<{
  host: RefObject<HTMLDivElement | null>;
  panes: Set<HTMLElement>;
}>;
export type PaneReadingPlace = Readonly<{ pane: HTMLElement; place: ReadingPlace }>;

export function capturePaneReadingPlaces(own: HTMLElement | null, group: PaneResizeGroup | null): readonly PaneReadingPlace[] {
  const panes = new Set(group?.panes);
  if (own !== null) panes.add(own);
  return [...panes].filter((pane) => pane.isConnected).map((pane) => ({ pane, place: readingPlaceIn(pane) }));
}

export function restorePaneReadingPlaces(places: readonly PaneReadingPlace[]): void {
  for (const { pane, place } of places) restoreReadingPlace(pane, place);
}
