import type { TrackFsCardMeta } from '../../api/generated-events';
import { cardRoleTones, ViewerChip } from '../chips';
import type { TrackFsViewer } from '../registry';
import { trackFsCardsIndexSchema } from '../schemas';

export const CardsIndexViewer: TrackFsViewer<TrackFsCardMeta[]> = {
  id: 'cards-index',
  match: (path) => path === 'cards/index.json',
  parse: (raw) => trackFsCardsIndexSchema.parse(JSON.parse(raw)),
  Component: CardsIndexViewerComponent,
};

function CardsIndexViewerComponent({
  data,
}: {
  data: TrackFsCardMeta[];
  path: string;
  raw: string;
}) {
  return (
    <section className="track-fs-viewer-cards-index">
      <h2 className="track-fs-viewer-title">
        Cards in this track ({data.length})
      </h2>
      {data.length === 0 ? (
        <p className="track-fs-viewer-empty">No cards in this track.</p>
      ) : (
        <ul className="track-fs-viewer-card-list">
          {data.map((item) => (
            <li className="track-fs-viewer-card-row" key={item.id}>
              <span className="track-fs-viewer-card-main">
                <span className="track-fs-viewer-card-title">
                  {item.kind}
                </span>
                <span className="track-fs-viewer-card-meta">
                  <span className="track-fs-viewer-card-id">
                    {item.id}
                  </span>
                  <ViewerChip label={item.role} tone={cardRoleTones[item.role]} />
                  <span className="track-fs-viewer-card-sort">
                    sort {item.sort}
                  </span>
                </span>
              </span>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
