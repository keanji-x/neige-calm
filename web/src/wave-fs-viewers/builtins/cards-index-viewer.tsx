import type { WaveFsViewer } from '../registry';

type CardsIndexItem = {
  kind: string;
  [key: string]: unknown;
};

export const CardsIndexViewer: WaveFsViewer<CardsIndexItem[]> = {
  id: 'cards-index',
  match: (path) => path === 'cards/index.json',
  parse: parseCardsIndex,
  Component: CardsIndexViewerComponent,
};

function parseCardsIndex(raw: string): CardsIndexItem[] {
  const parsed = JSON.parse(raw);
  if (!Array.isArray(parsed)) {
    throw new Error('cards/index.json must be an array');
  }
  parsed.forEach((item, index) => {
    if (!isCardsIndexItem(item)) {
      throw new Error(
        `cards/index.json item ${index} must include a kind string`,
      );
    }
  });
  return parsed;
}

function CardsIndexViewerComponent({
  data,
}: {
  data: CardsIndexItem[];
  path: string;
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
          {data.map((item, index) => (
            <li className="wave-fs-viewer-card-row" key={cardKey(item, index)}>
              <span className="wave-fs-viewer-card-main">
                <span className="wave-fs-viewer-card-title">
                  {item.kind}
                </span>
                <span className="wave-fs-viewer-card-meta">
                  <span className="wave-fs-viewer-card-id">
                    {stringField(item, 'id', 'missing-id')}
                  </span>
                  {stringField(item, 'role') ? (
                    <span className="wave-fs-viewer-card-role">
                      {stringField(item, 'role')}
                    </span>
                  ) : null}
                  <span className="wave-fs-viewer-card-sort">
                    sort {sortableField(item, 'sort')}
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

function stringField(
  item: CardsIndexItem,
  key: string,
  fallback = '',
): string {
  const value = field(item, key);
  return typeof value === 'string' && value.trim() ? value : fallback;
}

function sortableField(item: CardsIndexItem, key: string): string {
  const value = field(item, key);
  return typeof value === 'number' && Number.isFinite(value)
    ? String(value)
    : '-';
}

function cardKey(item: CardsIndexItem, index: number): string {
  const id = field(item, 'id');
  return typeof id === 'string' && id ? id : `card-${index}`;
}

function field(item: CardsIndexItem, key: string): unknown {
  return item[key];
}

function isCardsIndexItem(item: unknown): item is CardsIndexItem {
  if (typeof item !== 'object' || item === null) return false;
  const kind = (item as Record<string, unknown>).kind;
  return typeof kind === 'string' && kind.trim().length > 0;
}
