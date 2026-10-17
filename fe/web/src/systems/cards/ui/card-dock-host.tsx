import type { CardHost } from '../host.js';
import type { BoardHostItem } from './board-host.tsx';
import { CardCell } from './card-cell.tsx';

/** A retained single-card surface. The caller excludes these IDs from other hosts. */
export function CardDockHost({ host, item, visible, focused, onRemove }: {
  host: CardHost;
  item: BoardHostItem;
  visible: boolean;
  focused: boolean;
  onRemove?: () => void;
}) {
  return <div className="track-grid-wrap" hidden={!visible} inert={!visible} aria-hidden={!visible || undefined} data-nc-card-dock="" data-nc-card-id={item.card.id}>
    <div className="track-card"><CardCell host={host} item={item} visible={visible} focused={visible && focused} onRemove={item.deletable === false ? undefined : onRemove} /></div>
  </div>;
}
