import type { TrackFsRunIndexEntry } from '../../api/generated-events';
import { formatRelativeTime } from '../../shared/relativeTime';
import { runStatusTones, verdictTone, ViewerChip } from '../chips';
import type { TrackFsViewer } from '../registry';
import { trackFsRunsIndexSchema } from '../schemas';

export const RunsIndexViewer: TrackFsViewer<TrackFsRunIndexEntry[]> = {
  id: 'runs-index',
  match: (path) => path === 'runs/index.json',
  parse: (raw) => trackFsRunsIndexSchema.parse(JSON.parse(raw)),
  Component: RunsIndexViewerComponent,
};

function RunsIndexViewerComponent({
  data,
}: {
  data: TrackFsRunIndexEntry[];
  path: string;
  raw: string;
}) {
  return (
    <section className="track-fs-viewer-info-card">
      <h2 className="track-fs-viewer-title">Runs in this track ({data.length})</h2>
      {data.length === 0 ? (
        <p className="track-fs-viewer-empty">No runs yet.</p>
      ) : (
        <ul className="track-fs-viewer-list">
          {data.map((run) => (
            <li className="track-fs-viewer-row" key={run.idempotency_key}>
              <span className="track-fs-viewer-main">
                <span className="track-fs-viewer-primary">{run.kind}</span>
                <span className="track-fs-viewer-mono track-fs-viewer-small">
                  {run.idempotency_key}
                </span>
              </span>
              <span className="track-fs-viewer-meta">
                <ViewerChip label={run.status} tone={runStatusTones[run.status]} />
                {run.verdict ? (
                  <ViewerChip
                    label={run.verdict.status}
                    tone={verdictTone(run.verdict.status)}
                  />
                ) : null}
                <span className="track-fs-viewer-small">
                  {formatRelativeTime('Requested', run.requested_at)}
                </span>
                <span className="track-fs-viewer-small">
                  {formatRelativeTime('Finished', run.finished_at)}
                </span>
              </span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
