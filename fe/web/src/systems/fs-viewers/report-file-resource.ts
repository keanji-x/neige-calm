import { useEffect, useRef } from 'react';

import type { ApiFailure } from '../../../../core/api/types.ts';
import type { WorkspaceFilePort } from '../../../../core/domain/fs.ts';
import { readFailureOf } from '../../../../core/domain/read-failure.ts';
import { useState } from '../../ui/state/public.ts';
import { isImagePath, isMarkdownPath } from './file-kind.ts';

type ResourceState =
  | Readonly<{ kind: 'loading' }>
  | Readonly<{
      kind: 'loaded'; path: string; text: string; truncated: boolean;
      format: 'markdown' | 'source';
    }>
  | Readonly<{ kind: 'image'; path: string; url: string }>
  | Readonly<{ kind: 'error'; failure: ApiFailure | null; resource: 'file' | 'image' }>;

export type ReportFileResource =
  | Exclude<ResourceState, { kind: 'image' | 'error' }>
  | Readonly<{ kind: 'error'; failure: ApiFailure | null; resource: 'file' | 'image'; retry: () => void }>
  | Readonly<{
      kind: 'image'; path: string; url: string;
      onLoad: () => void;
      onError: () => void;
    }>;

/** Owns classification, async read/cancellation, and image load completion. */
export function useReportFileResource(
  path: string,
  files: WorkspaceFilePort,
  onOpened?: (path: string) => void,
): ReportFileResource {
  const onOpenedRef = useRef(onOpened);
  onOpenedRef.current = onOpened;
  const [retryKey, setRetryKey] = useState(0);
  const [state, setState] = useState<ResourceState>({ kind: 'loading' });

  useEffect(() => {
    if (isImagePath(path)) {
      setState({ kind: 'image', path, url: files.rawUrl(path) });
      return;
    }
    let cancelled = false;
    setState({ kind: 'loading' });
    files.readFile(path)
      .then((result) => {
        if (cancelled) return;
        setState({
          kind: 'loaded',
          path,
          text: result.text,
          truncated: result.truncated,
          format: isMarkdownPath(path) ? 'markdown' : 'source',
        });
        onOpenedRef.current?.(path);
      })
      .catch((error: unknown) => {
        if (!cancelled) setState({ kind: 'error', failure: readFailureOf(error), resource: 'file' });
      });
    return () => { cancelled = true; };
  }, [files, path, retryKey]);

  if (state.kind === 'error') return { ...state, retry: () => setRetryKey((value) => value + 1) };
  if (state.kind !== 'image') return state;
  return {
    ...state,
    onLoad: () => { onOpenedRef.current?.(state.path); },
    onError: () => { setState({ kind: 'error', failure: null, resource: 'image' }); },
  };
}
