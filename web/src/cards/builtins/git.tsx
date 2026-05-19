import type { GitCardData } from '../../types';
import type { CardEntry } from '../registry';

function GitCard({ card }: { card: GitCardData }) {
  return (
    <div className="git">
      <div className="git-head card-drag-handle">
        <span className="git-branch">{card.branch}</span>
        <span className="git-count">
          {card.commits.length} commit{card.commits.length > 1 ? 's' : ''}
        </span>
      </div>
      <div className="git-body">
        {card.commits.map((c, i) => (
          <div key={i} className="git-row">
            <span className="git-sha">{c.sha}</span>
            <span className="git-msg">{c.msg}</span>
            <span className="git-when">{c.when}</span>
          </div>
        ))}
      </div>
    </div>
  );
}

export const GitEntry: CardEntry<GitCardData> = {
  type: 'git',
  Component: GitCard,
  defaultSize: { w: 4, h: 7, minW: 3, minH: 4 },
  // No `addPanel`: git cards aren't user-created — they're seeded by future
  // git-plugin overlays. No `fromKernel` either: same reason.
};
