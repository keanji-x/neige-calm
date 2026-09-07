import { useCallback, useEffect } from 'react';
import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { taskArtifactFileOperation, taskFilePreview } from '../../../../core/domain/task-artifact-file.ts';
import { ArtifactFileDialog, type TaskFileView } from '../../features/report/task/artifact-file.tsx';
import { useState } from '../../ui/state/public.ts';
import { ApiError, runOperation } from '../providers/queries.ts';

export type TaskArtifactSelection = Readonly<{ taskKey: string; attemptId: string; index: number }>;

/** One file per Track view. File payloads never enter the session QueryClient cache. */
export function useTaskArtifactFiles({ trackId, transport, unauthorized }: {
  trackId: string; transport: ApiTransportPort; unauthorized: UnauthorizedChannel;
}) {
  const [selection, setSelection] = useState<TaskArtifactSelection | null>(null);
  const open = useCallback((next: TaskArtifactSelection) => setSelection(next), [setSelection]);
  return { open, dialog: selection === null ? null
    : <TaskArtifactFile key={JSON.stringify([trackId, selection])} trackId={trackId}
      selection={selection} transport={transport} unauthorized={unauthorized} onClose={() => setSelection(null)} /> };
}

type FileState = Exclude<TaskFileView, { phase: 'ready' }>
  | (Extract<TaskFileView, { phase: 'ready' }> & { url: string });
function TaskArtifactFile({ trackId, selection, transport, unauthorized, onClose }: {
  trackId: string; selection: TaskArtifactSelection; transport: ApiTransportPort; unauthorized: UnauthorizedChannel; onClose: () => void;
}) {
  const [view, setView] = useState<FileState>({ phase: 'loading' });
  const [retry, setRetry] = useState(0);
  const { taskKey, attemptId, index } = selection;
  useEffect(() => {
    const controller = new AbortController();
    let active = true;
    let url: string | null = null;
    setView({ phase: 'loading' });
    void runOperation(transport, { ...taskArtifactFileOperation(trackId, taskKey, attemptId, index), signal: controller.signal }, unauthorized)
      .then((file) => {
        if (!active) return;
        const raw = atob(file.contentBase64);
        if (raw.length !== file.size) throw new Error('Decoded file length does not match its declared size.');
        const bytes = Uint8Array.from(raw, (char) => char.charCodeAt(0));
        let preview = null;
        try { preview = taskFilePreview(new TextDecoder('utf-8', { fatal: true, ignoreBOM: true }).decode(bytes)); }
        catch { /* Invalid UTF-8 is a downloadable binary file. */ }
        url = URL.createObjectURL(new Blob([bytes], { type: 'application/octet-stream' }));
        setView({ phase: 'ready', name: file.name, size: file.size, preview, url });
      })
      .catch((error: unknown) => {
        if (active) setView({ phase: 'failed', message: error instanceof ApiError && error.failure.kind === 'decode'
          ? 'The file response could not be verified.' : error instanceof Error ? error.message : 'File is unavailable.' });
      });
    return () => {
      active = false;
      controller.abort();
      if (url !== null) URL.revokeObjectURL(url);
    };
  }, [trackId, taskKey, attemptId, index, transport, unauthorized, retry, setView]);
  const download = () => {
    if (view.phase !== 'ready') return;
    const link = document.createElement('a');
    link.href = view.url;
    link.download = view.name;
    document.body.append(link);
    try { link.click(); } finally { link.remove(); }
  };
  return <ArtifactFileDialog view={view} onClose={onClose} onRetry={() => setRetry((value) => value + 1)} onDownload={download} />;
}
