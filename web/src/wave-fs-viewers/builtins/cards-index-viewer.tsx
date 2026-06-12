import type { WaveFsCardMeta } from '../../api/generated-events';
import { cardRoleTones, ViewerChip } from '../chips';
import type { WaveFsViewer } from '../registry';
import { waveFsCardsIndexSchema } from '../schemas';

export const CardsIndexViewer: WaveFsViewer<WaveFsCardMeta[]> = {
  id: 'cards-index',
  match: (path) => path === 'cards/index.json',
  parse: (raw) => waveFsCardsIndexSchema.parse(JSON.parse(raw)),
  Component: CardsIndexViewerComponent,
};

function CardsIndexViewerComponent({
  data,
}: {
  data: WaveFsCardMeta[];
  path: string;
  raw: string;
}) {
  return (
    <section className="wave-fs-viewer-cards-index">
      <h2 className="wave-fs-viewer-title">
        Cards in this wave ({data.length})
      </h2>
      {data.length === 0 ? (
        <p className="wave-fs-viewer-empty">No cards in this wave.</p>
      ) : (
        <ul className="wave-fs-viewer-card-list">
          {data.map((item) => (
            <li className="wave-fs-viewer-card-row" key={item.id}>
              <span className="wave-fs-viewer-card-main">
                <span className="wave-fs-viewer-card-title">
                  {item.kind}
                </span>
                <span className="wave-fs-viewer-card-meta">
                  <span className="wave-fs-viewer-card-id">
                    {item.id}
                  </span>
                  <ViewerChip label={item.role} tone={cardRoleTones[item.role]} />
                  <span className="wave-fs-viewer-card-sort">
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
