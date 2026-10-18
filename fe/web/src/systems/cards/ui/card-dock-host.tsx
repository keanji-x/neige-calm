import type { CardHost } from '../host.js';
import type { BoardHostItem } from './board-host.tsx';
import type { CardHeaderPresentation } from '../registry.js';
import { CardCell } from './card-cell.tsx';

/** A retained single-card surface. The caller excludes these IDs from other hosts. */
export function CardDockHost({ host, item, visible, focused, onRemove, header }: {
  host: CardHost;
  item: BoardHostItem;
  visible: boolean;
  focused: boolean;
  onRemove?: () => void;
  header?: CardHeaderPresentation;
}) {
  return <div className="track-grid-wrap" hidden={!visible} inert={!visible} aria-hidden={!visible || undefined} data-nc-header-scope={header === undefined ? undefined : ''} data-nc-card-dock="" data-nc-card-id={item.card.id}>
    <div className="track-card"><CardCell header={header} host={host} item={item} visible={visible} focused={visible && focused} onRemove={item.deletable === false ? undefined : onRemove} /></div>
  </div>;
}
