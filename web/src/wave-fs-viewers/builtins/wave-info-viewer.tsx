import { formatRelativeTime } from '../../shared/relativeTime';
import { waveDisplayTitle } from '../../shared/waveTitle';
import type { WaveFsViewer } from '../registry';

type WaveInfo = {
  title: string;
  id: string;
  coveId: string;
  lifecycle: string;
  cwd?: string;
  sort?: number;
  archivedAt?: number;
  pinnedAt?: number;
};

export const WaveInfoViewer: WaveFsViewer<WaveInfo> = {
  id: 'wave-info',
  match: (path) => path === 'wave.json',
  parse: parseWaveInfo,
  Component: WaveInfoViewerComponent,
};

function parseWaveInfo(raw: string): WaveInfo {
  const parsed = JSON.parse(raw);
  if (!isRecord(parsed)) {
    throw new Error('wave.json must be an object');
  }
  return {
    title: requiredAnyString(parsed, 'title', 'wave.json'),
    id: requiredString(parsed, 'id', 'wave.json'),
    coveId: requiredString(parsed, 'cove_id', 'wave.json'),
    lifecycle: requiredString(parsed, 'lifecycle', 'wave.json'),
    cwd: optionalString(parsed, 'cwd'),
    sort: optionalNumber(parsed, 'sort'),
    archivedAt: optionalNumber(parsed, 'archived_at'),
    pinnedAt: optionalNumber(parsed, 'pinned_at'),
  };
}

function WaveInfoViewerComponent({
  data,
}: {
  data: WaveInfo;
  path: string;
  raw: string;
}) {
  return (
    <section className="wave-fs-viewer-info-card">
      <h2 className="wave-fs-viewer-primary">
        {waveDisplayTitle(data.title)}
      </h2>
      <div className="wave-fs-viewer-row">
        <span className="wave-fs-viewer-mono">{data.id}</span>
        <span className="wave-fs-viewer-mono">{data.coveId}</span>
        <ViewerChip label={data.lifecycle} tone={lifecycleTone(data.lifecycle)} />
        <span className="wave-fs-viewer-small">
          sort {formatNumber(data.sort)}
        </span>
      </div>
      <div className="wave-fs-viewer-field">
        <span className="wave-fs-viewer-label">cwd</span>
        <span className="wave-fs-viewer-mono wave-fs-viewer-break">
          {data.cwd || '-'}
        </span>
      </div>
      <div className="wave-fs-viewer-footer">
        {data.archivedAt !== undefined ? (
          <span>{formatRelativeTime('Archived', data.archivedAt)}</span>
        ) : null}
        {data.pinnedAt !== undefined ? (
          <span>{formatRelativeTime('Pinned', data.pinnedAt)}</span>
        ) : null}
      </div>
    </section>
  );
}

function ViewerChip({
  label,
  tone = 'neutral',
}: {
  label: string;
  tone?: string;
}) {
  return (
    <span className="wave-fs-viewer-chip" data-tone={tone}>
      {label}
    </span>
  );
}

function lifecycleTone(lifecycle: string): string {
  switch (lifecycle) {
    case 'active':
    case 'planning':
    case 'dispatching':
    case 'working':
    case 'reviewing':
      return 'accent';
    case 'blocked':
      return 'warning';
    case 'done':
      return 'success';
    case 'archived':
    case 'canceled':
    case 'failed':
      return 'danger';
    default:
      return 'neutral';
  }
}

function formatNumber(value: number | undefined): string {
  return value === undefined ? '-' : String(value);
}

function requiredAnyString(
  item: Record<string, unknown>,
  key: string,
  context: string,
): string {
  const value = item[key];
  if (typeof value !== 'string') {
    throw new Error(`${context} must include a ${key} string`);
  }
  return value;
}

function requiredString(
  item: Record<string, unknown>,
  key: string,
  context: string,
): string {
  const value = item[key];
  if (typeof value !== 'string' || value.trim().length === 0) {
    throw new Error(`${context} must include a ${key} string`);
  }
  return value;
}

function optionalString(
  item: Record<string, unknown>,
  key: string,
): string | undefined {
  const value = item[key];
  return typeof value === 'string' ? value : undefined;
}

function optionalNumber(
  item: Record<string, unknown>,
  key: string,
): number | undefined {
  const value = item[key];
  return typeof value === 'number' && Number.isFinite(value)
    ? value
    : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
