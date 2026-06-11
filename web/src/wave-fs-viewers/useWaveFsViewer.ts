import { useMemo, type FC } from 'react';
import { resolveWaveFsViewer } from './registry';

export function useWaveFsViewer(
  path: string,
  raw: string,
): {
  Viewer: FC<{ data: unknown; path: string; raw: string }>;
  data: unknown;
  raw: string;
} | null {
  return useMemo(() => {
    const viewer = resolveWaveFsViewer(path);
    if (!viewer) return null;

    try {
      return {
        Viewer: viewer.Component,
        data: viewer.parse(raw),
        raw,
      };
    } catch {
      return null;
    }
  }, [path, raw]);
}
