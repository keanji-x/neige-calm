// Wave fs viewers are optimistic by design: match a known path, parse the
// raw JSON, and render a richer view when that succeeds.
// We are not formalizing payload versions or migrations here.
// Any mismatch, drift, or malformed payload should throw from `parse`.
// Callers catch that failure and keep the raw CodeMirror JSON fallback intact.

import type { FC } from 'react';

export type WaveFsViewer<T> = {
  id: string;
  match: (path: string) => boolean;
  parse: (raw: string) => T;
  Component: FC<{ data: T; path: string }>;
};

const VIEWERS: Array<WaveFsViewer<unknown>> = [];

export function registerWaveFsViewer<T>(v: WaveFsViewer<T>): void {
  VIEWERS.push(v as unknown as WaveFsViewer<unknown>);
}

export function resolveWaveFsViewer(
  path: string,
): WaveFsViewer<unknown> | null {
  return VIEWERS.find((viewer) => viewer.match(path)) ?? null;
}

export function __resetWaveFsViewerRegistryForTest(): void {
  VIEWERS.length = 0;
}
