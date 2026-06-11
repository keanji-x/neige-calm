import type { WaveFsRunIndexEntry } from '../../api/generated-events';
import { formatRelativeTime } from '../../shared/relativeTime';
import { runStatusTones, verdictTone, ViewerChip } from '../chips';
import type { WaveFsViewer } from '../registry';
import { waveFsRunsIndexSchema } from '../schemas';

export const RunsIndexViewer: WaveFsViewer<WaveFsRunIndexEntry[]> = {
  id: 'runs-index',
  match: (path) => path === 'runs/index.json',
  parse: (raw) => waveFsRunsIndexSchema.parse(JSON.parse(raw)),
  Component: RunsIndexViewerComponent,
};

function RunsIndexViewerComponent({
  data,
}: {
  data: WaveFsRunIndexEntry[];
  path: string;
  raw: string;
}) {
  return (
    <section className="wave-fs-viewer-info-card">
      <h2 className="wave-fs-viewer-title">Runs in this wave ({data.length})</h2>
      {data.length === 0 ? (
        <p className="wave-fs-viewer-empty">No runs yet.</p>
      ) : (
        <ul className="wave-fs-viewer-list">
          {data.map((run) => (
            <li className="wave-fs-viewer-row" key={run.idempotency_key}>
              <span className="wave-fs-viewer-main">
                <span className="wave-fs-viewer-primary">{run.kind}</span>
                <span className="wave-fs-viewer-mono wave-fs-viewer-small">
                  {run.idempotency_key}
                </span>
              </span>
              <span className="wave-fs-viewer-meta">
                <ViewerChip label={run.status} tone={runStatusTones[run.status]} />
                {run.verdict ? (
                  <ViewerChip
                    label={run.verdict.status}
                    tone={verdictTone(run.verdict.status)}
                  />
                ) : null}
                <span className="wave-fs-viewer-small">
                  {formatRelativeTime('Requested', run.requested_at)}
                </span>
                <span className="wave-fs-viewer-small">
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
