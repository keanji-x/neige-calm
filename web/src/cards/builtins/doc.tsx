import type { DocCardData } from '../../types';
import type { CardEntry } from '../registry';

// Minimal inline markdown: **bold** and `code`. Lives next to DocCard
// because no other card needs it.
function inlineFmt(html: string): string {
  return html
    .replace(/\*\*(.+?)\*\*/g, '<strong>$1</strong>')
    .replace(/`([^`]+)`/g, '<code>$1</code>');
}

function DocCard({ card }: { card: DocCardData }) {
  return (
    <div className="doc">
      <div className="doc-head card-drag-handle">{card.title}</div>
      <div className="doc-body">
        {card.body.split('\n\n').map((para, i) => (
          <p key={i} dangerouslySetInnerHTML={{ __html: inlineFmt(para) }} />
        ))}
      </div>
    </div>
  );
}

export const DocEntry: CardEntry<DocCardData> = {
  type: 'doc',
  Component: DocCard,
  defaultSize: { w: 6, h: 7, minW: 3, minH: 3 },
  // Doc cards weren't user-creatable pre-M3, but Slice G's plugin-driven
  // AddPanel will read this metadata. No `fromKernel`: doc cards have no
  // built-in kernel kind yet — they only arrive as seed data today.
  addPanel: { label: 'New doc', icon: 'doc' },
};
