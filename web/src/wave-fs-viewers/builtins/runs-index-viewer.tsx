import { formatRelativeTime } from '../../shared/relativeTime';
import type { WaveFsViewer } from '../registry';

type RunVerdict = {
  status: string;
  at?: number;
};

type RunIndexEntry = {
  idempotencyKey: string;
  status: string;
  kind: string;
  verdict?: RunVerdict;
  requestedAt?: number;
  finishedAt?: number;
  workerCardId?: string;
};

export const RunsIndexViewer: WaveFsViewer<RunIndexEntry[]> = {
  id: 'runs-index',
  match: (path) => path === 'runs/index.json',
  parse: parseRunsIndex,
  Component: RunsIndexViewerComponent,
};

function parseRunsIndex(raw: string): RunIndexEntry[] {
  const parsed = JSON.parse(raw);
  if (!Array.isArray(parsed)) {
    throw new Error('runs/index.json must be an array');
  }
  return parsed.map((item, index) => {
    if (!isRecord(item)) {
      throw new Error(`runs/index.json item ${index} must be an object`);
    }
    return {
      idempotencyKey: requiredString(
        item,
        'idempotency_key',
        `runs/index.json item ${index}`,
      ),
      status: requiredString(item, 'status', `runs/index.json item ${index}`),
      kind: requiredString(item, 'kind', `runs/index.json item ${index}`),
      verdict: optionalVerdict(item, 'verdict', `runs/index.json item ${index}`),
      requestedAt: optionalNumber(item, 'requested_at'),
      finishedAt: optionalNumber(item, 'finished_at'),
      workerCardId: optionalNonEmptyString(item, 'worker_card_id'),
    };
  });
}

function RunsIndexViewerComponent({
  data,
}: {
  data: RunIndexEntry[];
  path: string;
}) {
  return (
    <section className="wave-fs-viewer-info-card">
      <h2 className="wave-fs-viewer-title">Runs in this wave ({data.length})</h2>
      {data.length === 0 ? (
        <p className="wave-fs-viewer-empty">No runs yet.</p>
      ) : (
        <ul className="wave-fs-viewer-list">
          {data.map((run) => (
            <li className="wave-fs-viewer-row" key={run.idempotencyKey}>
              <span className="wave-fs-viewer-main">
                <span className="wave-fs-viewer-primary">{run.kind}</span>
                <span className="wave-fs-viewer-mono wave-fs-viewer-small">
                  {run.idempotencyKey}
                </span>
              </span>
              <span className="wave-fs-viewer-meta">
                <ViewerChip label={run.status} tone={runStatusTone(run.status)} />
                {run.verdict ? (
                  <ViewerChip
                    label={run.verdict.status}
                    tone={verdictTone(run.verdict.status)}
                  />
                ) : null}
                <span className="wave-fs-viewer-small">
                  {formatRelativeTime('Requested', run.requestedAt)}
                </span>
                <span className="wave-fs-viewer-small">
                  {formatRelativeTime('Finished', run.finishedAt)}
                </span>
              </span>
            </li>
          ))}
        </ul>
      )}
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
    at: optionalNumber(value, 'at'),
  };
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
