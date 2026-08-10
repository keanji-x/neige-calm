// The conversation list — the second module in every route's panel card.
//
// It is the same on all three routes, which is the point: whatever you are
// looking at, the agent conversations attached to it are one click away in the
// same place. What changes per route is the *scope* the caller passes (this
// wave's, this cove's, everything), not this component.
//
// Presentational by construction: it fetches nothing and opens nothing. The
// caller owns the drawer, because the drawer overlays the whole main region and
// a module 308px wide has no business owning something that wide (§7.6).

import {
  byRecency, isLiveConversation, type Conversation,
} from '../../../../../core/domain/conversation.ts';
import { PanelEmpty } from '../../../ui/panel-card/public.tsx';
import styles from './list.module.css';

/** The label a session shows. `kind` is its identity, not decoration. */
const KIND_LABEL: Readonly<Record<Conversation['kind'], string>> = Object.freeze({
  terminal: 'Terminal',
  codex: 'Codex',
  claude: 'Claude',
  'shared-spec': 'Spec',
});

export type ChatListProps = Readonly<{
  conversations: readonly Conversation[];
  /** Which row is open in the drawer, if any. */
  activeId?: string | null;
  /** Whether to name the wave on each row — false when the page *is* a wave. */
  showWave?: boolean;
  onOpen: (conversation: Conversation) => void;
  nowMs?: number;
}>;

export function ChatList({
  conversations, activeId = null, showWave = true, onOpen, nowMs,
}: ChatListProps) {
  if (conversations.length === 0) {
    // No endpoint serves these yet — see the note in core/domain/conversation.ts.
    // One short sentence, no slice name, no apology (§5.3).
    return <PanelEmpty>No conversations yet.</PanelEmpty>;
  }

  const now = nowMs ?? Date.now();
  return (
    <ul className={styles.list}>
      {conversations.toSorted(byRecency).map((conversation) => {
        const live = isLiveConversation(conversation.state);
        const active = conversation.id === activeId;
        return (
          <li key={conversation.id}>
            <button
              type="button"
              data-nc-role="row"
              className={`${styles.row} ${active ? styles.rowActive : ''}`}
              aria-current={active ? 'true' : undefined}
              aria-label={`Conversation ${KIND_LABEL[conversation.kind]}`
                + (showWave ? `, on ${conversation.waveTitle}` : '')
                + `, ${conversation.turns} turns${live ? ', live' : ''}`}
              onClick={() => onOpen(conversation)}
            >
              {/* Live is the one state worth a colour here, and it takes the
                  same 6px dot a wave row uses for running — one vocabulary for
                  "something is happening", not a second one. */}
              <span
                className={`${styles.dot} ${live ? styles.dotLive : ''}`}
                aria-hidden="true"
              />
              <span className={styles.label}>
                {showWave ? conversation.waveTitle : KIND_LABEL[conversation.kind]}
              </span>
              <span className={styles.age}>{shortAge(conversation.updatedAt, now)}</span>
            </button>
          </li>
        );
      })}
    </ul>
  );
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** §2.2's relative time, floored to one unit. Duplicated nowhere: the wave
 *  row's copy lives in `features/wave/row` and features may not import a
 *  sibling domain, so this is the same *rule* re-declared, not a shared helper
 *  someone forgot to reuse. If a third surface needs it, it moves to `core`. */
function shortAge(atMs: number, nowMs: number): string {
  const elapsed = Math.max(0, nowMs - atMs);
  if (elapsed >= DAY) return `${Math.floor(elapsed / DAY)}d`;
  if (elapsed >= HOUR) return `${Math.floor(elapsed / HOUR)}h`;
  if (elapsed >= MINUTE) return `${Math.floor(elapsed / MINUTE)}m`;
  return 'now';
}
