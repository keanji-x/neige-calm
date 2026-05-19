import { z } from 'zod';
import type { DiffCardData } from '../../types';
import type { CardEntry } from '../registry';

/**
 * Wire shape for a `kind: "diff"` card's `payload`. Mirrors `DiffCardData`
 * minus the discriminator + id (those come from `KernelCard` itself).
 */
const diffLineSchema = z.object({
  kind: z.enum(['ctx', 'add', 'rm']),
  text: z.string(),
});

const diffHunkSchema = z.object({
  header: z.string(),
  lines: z.array(diffLineSchema),
});

const diffPayloadSchema = z.object({
  file: z.string(),
  added: z.number(),
  removed: z.number(),
  hunks: z.array(diffHunkSchema),
});

function DiffCard({ card }: { card: DiffCardData }) {
  return (
    <div className="diff">
      <div className="diff-head card-drag-handle">
        <span className="diff-file">{card.file}</span>
        <span className="diff-stats">
          <span className="diff-add">+{card.added}</span>
          <span className="diff-rm">−{card.removed}</span>
        </span>
      </div>
      <div className="diff-body">
        {card.hunks.map((h, i) => (
          <div key={i} className="diff-hunk">
            <div className="diff-line k-hdr">{h.header}</div>
            {h.lines.map((l, j) => (
              <div key={j} className={'diff-line k-' + l.kind}>
                <span className="diff-gutter">
                  {l.kind === 'add' ? '+' : l.kind === 'rm' ? '−' : ' '}
                </span>
                <span className="diff-text">{l.text}</span>
              </div>
            ))}
          </div>
        ))}
      </div>
    </div>
  );
}

export const DiffEntry: CardEntry<DiffCardData> = {
  type: 'diff',
  Component: DiffCard,
  defaultSize: { w: 6, h: 8, minW: 3, minH: 4 },
  // No `addPanel`: diff cards aren't user-created — they arrive from a
  // future git/PR plugin. The `fromKernel` adapter is here so when that
  // plugin path lands, the wire→UI hop is already schema-checked.
  fromKernel: (k) => {
    if (k.kind !== 'diff') return null;
    const parsed = diffPayloadSchema.safeParse(k.payload);
    if (!parsed.success) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] diff payload invalid for ${k.id}:`,
        parsed.error.issues,
      );
      return null;
    }
    return {
      type: 'diff',
      id: k.id,
      file: parsed.data.file,
      added: parsed.data.added,
      removed: parsed.data.removed,
      hunks: parsed.data.hunks,
    };
  },
};
