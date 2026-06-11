import { formatRelativeTime } from '../../shared/relativeTime';
import type { WaveFsViewer } from '../registry';

type RunVerdict = {
  status: string;
  reason?: string;
  at: number;
};

type RunDetail = {
  idempotencyKey: string;
  status: string;
  kind: string;
  verdict?: RunVerdict;
  requestedAt?: number;
  finishedAt?: number;
  workerCardId?: string;
};

export const RunDetailViewer: WaveFsViewer<RunDetail> = {
  id: 'run-detail',
  match: (path) =>
    path !== 'runs/index.json' && /^runs\/[^/]+\.json$/.test(path),
  parse: parseRunDetail,
  Component: RunDetailViewerComponent,
};

function parseRunDetail(raw: string): RunDetail {
  const parsed = JSON.parse(raw);
  if (!isRecord(parsed)) {
    throw new Error('runs/<key>.json must be an object');
  }
  return {
    idempotencyKey: requiredString(
      parsed,
      'idempotency_key',
      'runs/<key>.json',
    ),
    status: requiredString(parsed, 'status', 'runs/<key>.json'),
    kind: requiredString(parsed, 'kind', 'runs/<key>.json'),
    verdict: optionalVerdict(parsed, 'verdict', 'runs/<key>.json'),
    requestedAt: optionalNumber(parsed, 'requested_at'),
    finishedAt: optionalNumber(parsed, 'finished_at'),
    workerCardId: optionalNonEmptyString(parsed, 'worker_card_id'),
  };
}

function RunDetailViewerComponent({
  data,
  raw,
}: {
  data: RunDetail;
  path: string;
  raw: string;
}) {
  return (
    <section className="wave-fs-viewer-info-card">
      <h2 className="wave-fs-viewer-primary">{data.kind}</h2>
      <div className="wave-fs-viewer-row">
        <span className="wave-fs-viewer-mono">{data.idempotencyKey}</span>
      </div>
      <div className="wave-fs-viewer-row">
        <ViewerChip label={data.status} tone={runStatusTone(data.status)} />
        {data.verdict ? (
          <span className="wave-fs-viewer-verdict">
            <ViewerChip
              label={data.verdict.status}
              tone={verdictTone(data.verdict.status)}
            />
            {data.verdict.reason ? (
              <span className="wave-fs-viewer-verdict-reason">
                {data.verdict.reason}
              </span>
            ) : null}
          </span>
        ) : null}
      </div>
      <div className="wave-fs-viewer-footer">
        <span>{formatRelativeTime('Requested', data.requestedAt)}</span>
        <span>{formatRelativeTime('Finished', data.finishedAt)}</span>
      </div>
      {data.workerCardId ? (
        <div className="wave-fs-viewer-field">
          <span className="wave-fs-viewer-label">worker</span>
          <span className="wave-fs-viewer-mono">{data.workerCardId}</span>
        </div>
      ) : null}
      <details className="wave-fs-viewer-payload">
        <summary>Full payload (events, worker card)</summary>
        <pre className="wave-fs-viewer-payload-pre">
          <code>{raw}</code>
        </pre>
      </details>
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

function runStatusTone(status: string): string {
  switch (status) {
    case 'running':
    case 'requested':
      return 'accent';
    case 'completed':
      return 'success';
    case 'failed':
      return 'danger';
    default:
      return 'neutral';
  }
}

function verdictTone(status: string): string {
  switch (status) {
    case 'accepted':
    case 'approved':
    case 'completed':
    case 'done':
      return 'success';
    case 'rejected':
    case 'failed':
      return 'danger';
    case 'blocked':
      return 'warning';
    default:
      return 'neutral';
  }
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

function optionalNonEmptyString(
  item: Record<string, unknown>,
  key: string,
): string | undefined {
  const value = item[key];
  return typeof value === 'string' && value.trim().length > 0
    ? value
    : undefined;
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

function requiredNumber(
  item: Record<string, unknown>,
  key: string,
  context: string,
): number {
  const value = item[key];
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    throw new Error(`${context} must include a ${key} number`);
  }
  return value;
}

function optionalVerdict(
  item: Record<string, unknown>,
  key: string,
  context: string,
): RunVerdict | undefined {
  const value = item[key];
  if (value == null) return undefined;
  if (!isRecord(value)) {
    throw new Error(`${context} verdict must be null or an object`);
  }
  return {
    status: requiredString(value, 'status', `${context} verdict`),
    reason: optionalNonEmptyString(value, 'reason'),
    at: requiredNumber(value, 'at', `${context} verdict`),
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
