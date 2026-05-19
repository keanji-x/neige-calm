import type { DiffCardData } from '../../types';
import type { CardEntry } from '../registry';

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
  // future git/PR plugin. No `fromKernel`: same.
};
