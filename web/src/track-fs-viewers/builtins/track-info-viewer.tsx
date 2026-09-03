import type { Track } from '../../api/generated-events';
import { formatRelativeTime } from '../../shared/relativeTime';
import { trackDisplayTitle } from '../../shared/trackTitle';
import { ViewerChip, trackLifecycleTones } from '../chips';
import type { TrackFsViewer } from '../registry';
import { trackFsTrackSchema } from '../schemas';

export const TrackInfoViewer: TrackFsViewer<Track> = {
  id: 'track-info',
  match: (path) => path === 'track.json',
  parse: (raw) => trackFsTrackSchema.parse(JSON.parse(raw)),
  Component: TrackInfoViewerComponent,
};

function TrackInfoViewerComponent({
  data,
}: {
  data: Track;
  path: string;
  raw: string;
}) {
  return (
    <section className="track-fs-viewer-info-card">
      <h2 className="track-fs-viewer-primary">
        {trackDisplayTitle(data.title)}
      </h2>
      <div className="track-fs-viewer-row">
        <span className="track-fs-viewer-mono">{data.id}</span>
        <span className="track-fs-viewer-mono">{data.area_id}</span>
        <ViewerChip
          label={data.lifecycle}
          tone={trackLifecycleTones[data.lifecycle]}
        />
        <span className="track-fs-viewer-small">
          sort {data.sort}
        </span>
      </div>
      <div className="track-fs-viewer-field">
        <span className="track-fs-viewer-label">cwd</span>
        <span className="track-fs-viewer-mono track-fs-viewer-break">
          {data.cwd || '-'}
        </span>
      </div>
      <div className="track-fs-viewer-footer">
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
