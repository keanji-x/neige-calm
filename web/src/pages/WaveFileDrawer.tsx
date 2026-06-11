import { useEffect, useMemo, useRef } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { CalmApiError } from '../api/calm';
import { useWaveFileContent } from '../api/queries';
import { useTheme } from '../app/theme';
import { CodePane } from '../cards/builtins/file-viewer-codemirror';
import { useState } from '../shared/state';

export interface WaveFileDrawerProps {
  waveId: string;
  path: string;
  onClose: () => void;
}

export function WaveFileDrawer({ waveId, path, onClose }: WaveFileDrawerProps) {
  const backdropRef = useRef<HTMLDivElement | null>(null);
  const [entered, setEntered] = useState(false);
  const { resolved: theme } = useTheme();
  const contentQ = useWaveFileContent(waveId, path, { enabled: true });
  const title = useMemo(() => leafName(path), [path]);

  useEffect(() => {
    const cancel = scheduleFrame(() => setEntered(true));
    return cancel;
  }, []);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => {
      window.removeEventListener('keydown', handleKeyDown);
    };
  }, [onClose]);

  useEffect(() => {
    const backdrop = backdropRef.current;
    if (!backdrop) return;
    const handleMouseDown = (event: MouseEvent) => {
      if (event.target === backdrop) onClose();
    };
    backdrop.addEventListener('mousedown', handleMouseDown);
    return () => {
      backdrop.removeEventListener('mousedown', handleMouseDown);
    };
  }, [onClose]);

  return (
    <div ref={backdropRef} className="wave-file-drawer-backdrop">
      {/* eslint-disable neige-calm/no-raw-primitive-role -- This drawer is intentionally non-modal and must not use Dialog's focus trap. */}
      <section
        role="dialog"
        aria-modal="false"
        aria-label={`File viewer: ${path}`}
        className={entered ? 'wave-file-drawer is-open' : 'wave-file-drawer'}
      >
        <header className="wave-file-drawer-header">
          <div className="wave-file-drawer-heading">
            <h2 className="wave-file-drawer-title">{title}</h2>
            <div className="wave-file-drawer-path">{path}</div>
          </div>
          <button
            type="button"
            className="wave-file-drawer-close"
            aria-label="Close file viewer"
            onClick={onClose}
          >
            Close
          </button>
        </header>
        {/* eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- The drawer body must be keyboard-focusable so it can scroll without moving focus into content. */}
        <div className="wave-file-drawer-body" tabIndex={0}>
          <WaveFileDrawerBody path={path} contentQ={contentQ} theme={theme} />
        </div>
      </section>
      {/* eslint-enable neige-calm/no-raw-primitive-role */}
    </div>
  );
}

function WaveFileDrawerBody({
  path,
  contentQ,
  theme,
}: {
  path: string;
  contentQ: ReturnType<typeof useWaveFileContent>;
  theme: 'light' | 'dark';
}) {
  if (contentQ.isLoading) {
    return <div className="wave-file-drawer-empty">Loading...</div>;
  }
  if (contentQ.error) {
    return <InlineApiError error={contentQ.error} />;
  }
  if (!contentQ.data) {
    return <div className="wave-file-drawer-empty">Select a file</div>;
  }

  if (contentQ.data.content_type === 'text/markdown') {
    return (
      <div className="wave-file-drawer-markdown report-prose">
        <ReactMarkdown remarkPlugins={[remarkGfm]}>
          {contentQ.data.content}
        </ReactMarkdown>
      </div>
    );
  }

  if (isTextContent(contentQ.data.content_type)) {
    return (
      <div className="wave-file-drawer-code">
        <CodePane path={path} text={contentQ.data.content} theme={theme} />
      </div>
    );
  }

  return (
    <div className="wave-file-drawer-empty">
      Preview unavailable for {contentQ.data.content_type}
    </div>
  );
}

function InlineApiError({ error }: { error: Error }) {
  return (
    <div role="alert" className="wave-file-drawer-error">
      {formatApiError(error)}
    </div>
  );
}

function formatApiError(error: Error): string {
  if (error instanceof CalmApiError) {
    return error.message || error.code || `HTTP ${error.status}`;
  }
  return error.message || 'Request failed';
}

function leafName(path: string): string {
  return path.split('/').filter(Boolean).pop() ?? path;
}

function isTextContent(contentType: string): boolean {
  return (
    contentType.startsWith('text/') ||
    contentType === 'application/json' ||
    contentType.endsWith('+json') ||
    contentType === 'application/xml' ||
    contentType.endsWith('+xml')
  );
}

function scheduleFrame(callback: FrameRequestCallback): () => void {
  if (typeof window.requestAnimationFrame === 'function') {
    const frameId = window.requestAnimationFrame(callback);
    return () => window.cancelAnimationFrame(frameId);
  }
  const timeoutId = window.setTimeout(callback, 0);
  return () => window.clearTimeout(timeoutId);
}
