import type { TrackFsRunDetail } from '../../api/generated-events';
import { formatRelativeTime } from '../../shared/relativeTime';
import { runStatusTones, verdictTone, ViewerChip } from '../chips';
import type { TrackFsViewer } from '../registry';
import { trackFsRunDetailSchema } from '../schemas';

export const RunDetailViewer: TrackFsViewer<TrackFsRunDetail> = {
  id: 'run-detail',
  match: (path) =>
    path !== 'runs/index.json' && /^runs\/[^/]+\.json$/.test(path),
  parse: (raw) => trackFsRunDetailSchema.parse(JSON.parse(raw)),
  Component: RunDetailViewerComponent,
};

function RunDetailViewerComponent({
  data,
  raw,
}: {
  data: TrackFsRunDetail;
  path: string;
  raw: string;
}) {
  return (
    <section className="track-fs-viewer-info-card">
      <h2 className="track-fs-viewer-primary">{data.kind}</h2>
      <div className="track-fs-viewer-row">
        <span className="track-fs-viewer-mono">{data.idempotency_key}</span>
      </div>
      <div className="track-fs-viewer-row">
        <ViewerChip label={data.status} tone={runStatusTones[data.status]} />
        {data.verdict ? (
          <span className="track-fs-viewer-verdict">
            <ViewerChip
              label={data.verdict.status}
              tone={verdictTone(data.verdict.status)}
            />
            {data.verdict.reason ? (
              <span className="track-fs-viewer-verdict-reason">
                {data.verdict.reason}
              </span>
            ) : null}
          </span>
        ) : null}
      </div>
      <div className="track-fs-viewer-footer">
        <span>{formatRelativeTime('Requested', data.requested_at)}</span>
        <span>{formatRelativeTime('Finished', data.finished_at)}</span>
      </div>
      {data.worker_card_id ? (
        <div className="track-fs-viewer-field">
          <span className="track-fs-viewer-label">worker</span>
          <span className="track-fs-viewer-mono">{data.worker_card_id}</span>
        </div>
      ) : null}
      <details className="track-fs-viewer-payload">
        <summary>Full payload (events, worker card)</summary>
        <pre className="track-fs-viewer-payload-pre">
          <code>{raw}</code>
        </pre>
      </details>
    </section>
  );
}
