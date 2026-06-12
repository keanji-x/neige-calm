import type { WaveFsCardMeta } from '../../api/generated-events';
import { formatRelativeTime } from '../../shared/relativeTime';
import { cardRoleTones, ViewerChip } from '../chips';
import type { WaveFsViewer } from '../registry';
import { waveFsCardMetaSchema } from '../schemas';

export const CardMetaViewer: WaveFsViewer<WaveFsCardMeta> = {
  id: 'card-meta',
  match: (path) => /^cards\/[^/]+\/\.meta\.json$/.test(path),
  parse: (raw) => waveFsCardMetaSchema.parse(JSON.parse(raw)),
  Component: CardMetaViewerComponent,
};

function CardMetaViewerComponent({
  data,
}: {
  data: WaveFsCardMeta;
  path: string;
  raw: string;
}) {
  return (
    <section className="wave-fs-viewer-info-card">
      <h2 className="wave-fs-viewer-primary">{data.kind}</h2>
      <div className="wave-fs-viewer-row">
        <span className="wave-fs-viewer-mono">{data.id}</span>
        <ViewerChip label={data.role} tone={cardRoleTones[data.role]} />
        <span className="wave-fs-viewer-small">sort {data.sort}</span>
      </div>
      <div className="wave-fs-viewer-footer">
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
