import type { Wave } from '../../api/generated-events';
import { formatRelativeTime } from '../../shared/relativeTime';
import { waveDisplayTitle } from '../../shared/waveTitle';
import { ViewerChip, waveLifecycleTones } from '../chips';
import type { WaveFsViewer } from '../registry';
import { waveFsWaveSchema } from '../schemas';

export const WaveInfoViewer: WaveFsViewer<Wave> = {
  id: 'wave-info',
  match: (path) => path === 'wave.json',
  parse: (raw) => waveFsWaveSchema.parse(JSON.parse(raw)),
  Component: WaveInfoViewerComponent,
};

function WaveInfoViewerComponent({
  data,
}: {
  data: Wave;
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
        <span className="wave-fs-viewer-mono">{data.cove_id}</span>
        <ViewerChip
          label={data.lifecycle}
          tone={waveLifecycleTones[data.lifecycle]}
        />
        <span className="wave-fs-viewer-small">
          sort {data.sort}
        </span>
      </div>
      <div className="wave-fs-viewer-field">
        <span className="wave-fs-viewer-label">cwd</span>
        <span className="wave-fs-viewer-mono wave-fs-viewer-break">
          {data.cwd || '-'}
        </span>
      </div>
      <div className="wave-fs-viewer-footer">
        {data.archived_at !== null ? (
          <span>{formatRelativeTime('Archived', data.archived_at)}</span>
        ) : null}
        {data.pinned_at !== null ? (
          <span>{formatRelativeTime('Pinned', data.pinned_at)}</span>
        ) : null}
      </div>
    </section>
  );
}
