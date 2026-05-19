import { z } from 'zod';
import type { GitCardData } from '../../types';
import type { CardEntry } from '../registry';

/**
 * Wire shape for a `kind: "git"` card's `payload`. Git cards are seeded by
 * a future git-plugin overlay; the schema is here so the writer doesn't
 * have to invent it twice.
 */
const gitCommitSchema = z.object({
  sha: z.string(),
  msg: z.string(),
  when: z.string(),
});

const gitPayloadSchema = z.object({
  branch: z.string(),
  commits: z.array(gitCommitSchema),
});

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
  // git-plugin overlays. The `fromKernel` adapter is scoped to
  // `kind === 'git'` so it stays a no-op until that plugin path exists.
  fromKernel: (k) => {
    if (k.kind !== 'git') return null;
    const parsed = gitPayloadSchema.safeParse(k.payload);
    if (!parsed.success) {
      // eslint-disable-next-line no-console
      console.warn(
        `[cards] git payload invalid for ${k.id}:`,
        parsed.error.issues,
      );
      return null;
    }
    return {
      type: 'git',
      id: k.id,
      branch: parsed.data.branch,
      commits: parsed.data.commits,
    };
  },
};
