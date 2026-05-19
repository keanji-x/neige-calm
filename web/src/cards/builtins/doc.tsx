import { z } from 'zod';
import type { DocCardData } from '../../types';
import type { CardEntry } from '../registry';

/**
 * Wire shape for a `kind: "doc"` card's `payload`. Doc cards are seed-only
 * today (no creation path through the kernel), but defining the schema
 * here lets a future writer drop the cast and lets us gate any kernel-
 * sourced doc through `fromKernel` below without changing this file again.
 */
const docPayloadSchema = z.object({
  title: z.string(),
  body: z.string(),
});

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
  // AddPanel will read this metadata. The `fromKernel` adapter only fires
  // for `kind === 'doc'` — today no server path produces that, but having
  // the schema-gated branch here means we won't be carrying a dead cast
  // when one lands.
  fromKernel: (k) => {
    if (k.kind !== 'doc') return null;
    const parsed = docPayloadSchema.safeParse(k.payload);
    if (!parsed.success) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] doc payload invalid for ${k.id}:`,
        parsed.error.issues,
      );
      return null;
    }
    return {
      type: 'doc',
      id: k.id,
      title: parsed.data.title,
      body: parsed.data.body,
    };
  },
  addPanel: { label: 'New doc', icon: 'doc' },
};
