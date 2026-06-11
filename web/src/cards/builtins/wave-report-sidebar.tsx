import type { ReactNode } from 'react';
import ReactMarkdown from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { CalmApiError } from '../../api/calm';
import { useWaveFileContent } from '../../api/queries';
import { useTheme } from '../../app/theme';
import { WaveFileTree } from '../wave-file-tree';
import { CodePane } from './file-viewer-codemirror';

export interface WaveReportSidebarProps {
  waveId: string;
  selectedPath: string | null;
  onSelectedPathChange: (path: string | null) => void;
  fallback?: ReactNode;
}

export function WaveReportSidebar({
  waveId,
  selectedPath,
  onSelectedPathChange,
  fallback,
}: WaveReportSidebarProps) {
  return (
    <WaveReportSidebarState
      key={waveId}
      waveId={waveId}
      selectedPath={selectedPath}
      onSelectedPathChange={onSelectedPathChange}
      fallback={fallback}
    />
  );
}

function WaveReportSidebarState({
  waveId,
  selectedPath,
  onSelectedPathChange,
  fallback,
}: WaveReportSidebarProps) {
  return (
    <div className="wave-report-files" data-testid="wave-report-files">
      <WaveFileTree
        waveId={waveId}
        selectedPath={selectedPath}
        onSelectedPathChange={onSelectedPathChange}
      />
      <WaveFileViewer
        waveId={waveId}
        selectedPath={selectedPath}
        fallback={fallback}
      />
    </div>
  );
}

function WaveFileViewer({
  waveId,
  selectedPath,
  fallback,
}: {
  waveId: string;
  selectedPath: string | null;
  fallback?: ReactNode;
}) {
  const { resolved: theme } = useTheme();
  const contentQ = useWaveFileContent(waveId, selectedPath);

  if (!selectedPath) {
    if (fallback) {
      return <div className="wave-report-files-viewer">{fallback}</div>;
    }
    return (
      <div className="wave-report-files-viewer wave-report-files-viewer-empty">
        Select a file
      </div>
    );
  }
  if (contentQ.isLoading) {
    return (
      <div className="wave-report-files-viewer wave-report-files-viewer-empty">
        Loading...
      </div>
    );
  }
  if (contentQ.error) {
    return (
      <div className="wave-report-files-viewer">
        <InlineApiError error={contentQ.error} />
      </div>
    );
  }
  if (!contentQ.data) {
    return (
      <div className="wave-report-files-viewer wave-report-files-viewer-empty">
        Select a file
      </div>
    );
  }

  if (contentQ.data.content_type === 'text/markdown') {
    return (
      <div className="wave-report-files-viewer wave-report-files-markdown">
        <ReactMarkdown remarkPlugins={[remarkGfm]}>
          {contentQ.data.content}
        </ReactMarkdown>
      </div>
    );
  }

  return (
    <div className="wave-report-files-viewer wave-report-files-code-wrap">
      <CodePane path={selectedPath} text={contentQ.data.content} theme={theme} />
    </div>
  );
}

function InlineApiError({ error }: { error: Error }) {
  return (
    <div role="alert" className="wave-report-files-error">
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
