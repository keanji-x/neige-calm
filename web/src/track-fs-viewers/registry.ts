// Track fs viewers are optimistic by design: match a known path, parse the
// raw JSON, and render a richer view when that succeeds.
// We are not formalizing payload versions or migrations here.
// Any mismatch, drift, or malformed payload should throw from `parse`.
// Callers catch that failure and keep the raw CodeMirror JSON fallback intact.

import type { FC } from 'react';

export type TrackFsViewer<T> = {
  id: string;
  match: (path: string) => boolean;
  parse: (raw: string) => T;
  Component: FC<{ data: T; path: string; raw: string }>;
};

const VIEWERS: Array<TrackFsViewer<unknown>> = [];

export function registerTrackFsViewer<T>(v: TrackFsViewer<T>): void {
  VIEWERS.push(v as unknown as TrackFsViewer<unknown>);
}

export function resolveTrackFsViewer(
  path: string,
): TrackFsViewer<unknown> | null {
  return VIEWERS.find((viewer) => viewer.match(path)) ?? null;
}

export function __resetTrackFsViewerRegistryForTest(): void {
  VIEWERS.length = 0;
}
