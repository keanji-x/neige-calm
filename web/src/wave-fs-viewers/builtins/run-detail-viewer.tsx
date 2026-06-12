import type { WaveFsRunDetail } from '../../api/generated-events';
import { formatRelativeTime } from '../../shared/relativeTime';
import { runStatusTones, verdictTone, ViewerChip } from '../chips';
import type { WaveFsViewer } from '../registry';
import { waveFsRunDetailSchema } from '../schemas';

export const RunDetailViewer: WaveFsViewer<WaveFsRunDetail> = {
  id: 'run-detail',
  match: (path) =>
    path !== 'runs/index.json' && /^runs\/[^/]+\.json$/.test(path),
  parse: (raw) => waveFsRunDetailSchema.parse(JSON.parse(raw)),
  Component: RunDetailViewerComponent,
};

function RunDetailViewerComponent({
  data,
  raw,
}: {
  data: WaveFsRunDetail;
  path: string;
  raw: string;
}) {
  return (
    <section className="wave-fs-viewer-info-card">
      <h2 className="wave-fs-viewer-primary">{data.kind}</h2>
      <div className="wave-fs-viewer-row">
        <span className="wave-fs-viewer-mono">{data.idempotency_key}</span>
      </div>
      <div className="wave-fs-viewer-row">
        <ViewerChip label={data.status} tone={runStatusTones[data.status]} />
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
        <span>{formatRelativeTime('Requested', data.requested_at)}</span>
        <span>{formatRelativeTime('Finished', data.finished_at)}</span>
      </div>
      {data.worker_card_id ? (
        <div className="wave-fs-viewer-field">
          <span className="wave-fs-viewer-label">worker</span>
          <span className="wave-fs-viewer-mono">{data.worker_card_id}</span>
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
