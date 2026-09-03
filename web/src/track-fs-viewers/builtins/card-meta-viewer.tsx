import type { TrackFsCardMeta } from '../../api/generated-events';
import { formatRelativeTime } from '../../shared/relativeTime';
import { cardRoleTones, ViewerChip } from '../chips';
import type { TrackFsViewer } from '../registry';
import { trackFsCardMetaSchema } from '../schemas';

export const CardMetaViewer: TrackFsViewer<TrackFsCardMeta> = {
  id: 'card-meta',
  match: (path) => /^cards\/[^/]+\/\.meta\.json$/.test(path),
  parse: (raw) => trackFsCardMetaSchema.parse(JSON.parse(raw)),
  Component: CardMetaViewerComponent,
};

function CardMetaViewerComponent({
  data,
}: {
  data: TrackFsCardMeta;
  path: string;
  raw: string;
}) {
  return (
    <section className="track-fs-viewer-info-card">
      <h2 className="track-fs-viewer-primary">{data.kind}</h2>
      <div className="track-fs-viewer-row">
        <span className="track-fs-viewer-mono">{data.id}</span>
        <ViewerChip label={data.role} tone={cardRoleTones[data.role]} />
        <span className="track-fs-viewer-small">sort {data.sort}</span>
      </div>
      <div className="track-fs-viewer-footer">
        <span>{formatRelativeTime('Created', data.created_at)}</span>
        <span>{formatRelativeTime('Updated', data.updated_at)}</span>
        <span>deletable: {formatBoolean(data.deletable)}</span>
      </div>
    </section>
  );
}

function formatBoolean(value: boolean): string {
  return value ? 'yes' : 'no';
}
