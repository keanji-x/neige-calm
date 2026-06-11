import { formatRelativeTime } from '../../shared/relativeTime';
import type { WaveFsViewer } from '../registry';

type CardMeta = {
  id: string;
  kind: string;
  role?: string;
  sort?: number;
  deletable?: boolean;
  createdAt?: number;
  updatedAt?: number;
};

export const CardMetaViewer: WaveFsViewer<CardMeta> = {
  id: 'card-meta',
  match: (path) => /^cards\/[^/]+\/meta\.json$/.test(path),
  parse: parseCardMeta,
  Component: CardMetaViewerComponent,
};

function parseCardMeta(raw: string): CardMeta {
  const parsed = JSON.parse(raw);
  if (!isRecord(parsed)) {
    throw new Error('cards/<id>/meta.json must be an object');
  }
  return {
    id: requiredString(parsed, 'id', 'cards/<id>/meta.json'),
    kind: requiredString(parsed, 'kind', 'cards/<id>/meta.json'),
    role: optionalNonEmptyString(parsed, 'role'),
    sort: optionalNumber(parsed, 'sort'),
    deletable: optionalBoolean(parsed, 'deletable'),
    createdAt: optionalNumber(parsed, 'created_at'),
    updatedAt: optionalNumber(parsed, 'updated_at'),
  };
}

function CardMetaViewerComponent({
  data,
}: {
  data: CardMeta;
  path: string;
}) {
  return (
    <section className="wave-fs-viewer-info-card">
      <h2 className="wave-fs-viewer-primary">{data.kind}</h2>
      <div className="wave-fs-viewer-row">
        <span className="wave-fs-viewer-mono">{data.id}</span>
        {data.role ? <ViewerChip label={data.role} /> : null}
        <span className="wave-fs-viewer-small">
          sort {formatNumber(data.sort)}
        </span>
      </div>
      <div className="wave-fs-viewer-footer">
        <span>{formatRelativeTime('Created', data.createdAt)}</span>
        <span>{formatRelativeTime('Updated', data.updatedAt)}</span>
        <span>deletable: {formatBoolean(data.deletable)}</span>
      </div>
    </section>
  );
}

function ViewerChip({ label }: { label: string }) {
  return <span className="wave-fs-viewer-chip">{label}</span>;
}

function formatNumber(value: number | undefined): string {
  return value === undefined ? '-' : String(value);
}

function formatBoolean(value: boolean | undefined): string {
  if (value === undefined) return '-';
  return value ? 'yes' : 'no';
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

function optionalBoolean(
  item: Record<string, unknown>,
  key: string,
): boolean | undefined {
  const value = item[key];
  return typeof value === 'boolean' ? value : undefined;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}
