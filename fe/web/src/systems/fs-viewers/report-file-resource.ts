import { useEffect, useRef } from 'react';

import type { WorkspaceFilePort } from '../../../../core/domain/fs.ts';
import { useState } from '../../ui/state/public.ts';
import { isImagePath, isMarkdownPath } from './file-kind.ts';

type ResourceState =
  | Readonly<{ kind: 'loading' }>
  | Readonly<{
      kind: 'loaded'; path: string; text: string; truncated: boolean;
      format: 'markdown' | 'source';
    }>
  | Readonly<{ kind: 'image'; path: string; url: string }>
  | Readonly<{ kind: 'error'; message: string }>;

export type ReportFileResource =
  | Exclude<ResourceState, { kind: 'image' }>
  | Readonly<{
      kind: 'image'; path: string; url: string;
      onLoad: () => void;
      onError: () => void;
    }>;

function messageOf(error: unknown): string {
  return error instanceof Error && error.message !== '' ? error.message : 'Could not read this file.';
}

/** Owns classification, async read/cancellation, and image load completion. */
export function useReportFileResource(
  path: string,
  files: WorkspaceFilePort,
  onOpened?: (path: string) => void,
): ReportFileResource {
  const onOpenedRef = useRef(onOpened);
  onOpenedRef.current = onOpened;
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
        if (!cancelled) setState({ kind: 'error', message: messageOf(error) });
      });
    return () => { cancelled = true; };
  }, [files, path]);

  if (state.kind !== 'image') return state;
  return {
    ...state,
    onLoad: () => { onOpenedRef.current?.(state.path); },
    onError: () => { setState({ kind: 'error', message: 'Could not read this image.' }); },
  };
}
