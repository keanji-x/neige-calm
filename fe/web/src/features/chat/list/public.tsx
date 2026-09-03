// The conversation list — the second module in every route's panel card.
//
// It is the same on all three routes, which is the point: whatever you are
// looking at, the agent conversations attached to it are one click away in the
// same place. What changes per route is the *scope* the caller passes (this
// wave's, this area's, everything), not this component.
//
// Presentational by construction: it fetches nothing and opens nothing. The
// caller owns the drawer, because the drawer overlays the whole main region and
// a module 308px wide has no business owning something that wide (§7.6).

import {
  byRecency, conversationName, isLiveConversation, type Conversation,
} from '../../../../../core/domain/conversation.ts';
import { PanelEmpty } from '../../../ui/panel-card/public.tsx';
import styles from './list.module.css';

export type ChatListProps = Readonly<{
  conversations: readonly Conversation[];
  /** Which row is open in the drawer, if any. */
  activeId?: string | null;
  /** Whether to name the wave on each row — false when the page *is* a wave. */
  showWave?: boolean;
  onOpen: (conversation: Conversation) => void;
}>;

export function ChatList({
  conversations, activeId = null, showWave = true, onOpen,
}: ChatListProps) {
  if (conversations.length === 0) {
    // One short sentence, no slice name, no apology (§5.3).
    return <PanelEmpty>No conversations yet.</PanelEmpty>;
  }

  return (
    <ul className={styles.list}>
      {conversations.toSorted(byRecency).map((conversation) => {
        const live = isLiveConversation(conversation.state);
        const active = conversation.id === activeId;
        /* Both are optional and both are said only when known: a row whose wave
           has no title the reader may see says nothing about a wave, and a row
           with no turn count says nothing about turns. Interpolating them
           unguarded reads "on undefined" / "undefined turns"; defaulting them
           to `''` and `0` is worse, because `0 turns` is a claim. */
        const waveTitle = conversation.waveTitle;
        const turns = conversation.turns;
        const name = conversationName(conversation);
        return (
          <li key={conversation.id} className={styles.item}>
            <button
              type="button"
              data-nc-role="row"
              className={`${styles.row} ${active ? styles.rowActive : ''}`}
              aria-current={active ? 'true' : undefined}
              aria-label={`Conversation ${name}`
                + (showWave && waveTitle !== undefined ? `, on ${waveTitle}` : '')
                + (turns === undefined ? '' : `, ${turns} turns`)
                + (live ? ', live' : '')}
              onClick={() => onOpen(conversation)}
            >
              {/* Both, never one instead of the other (#1189 §5).
                  The row used to render the wave's title *in place of* the
                  conversation's whenever it knew one, which was unambiguous
                  only while a wave could contribute at most one row to this
                  list. It can now contribute all of them, and N rows reading
                  `Test wave` are N rows a sighted reader cannot tell apart —
                  the difference lived in the `aria-label` alone. So the name
                  is the row and the wave is what follows it, quieter and
                  second, which is also the order the label says them in. */}
              <span className={styles.label}>
                <span className={styles.name}>{name}</span>
                {showWave && waveTitle !== undefined && (
                  <span className={styles.wave}>{waveTitle}</span>
                )}
              </span>
            </button>
            {/* Trailing, outside the button — the same shape a wave row takes in
                a panel, and for the same reasons: the module head's `+` already
                owns this column, and a 308px row cannot spend width on both a
                leading state cell and a trailing age. Live is the one state
                worth a colour, and it takes the same 6px dot a wave row uses
                for running — one vocabulary for "something is happening". */}
            <span
              className={`${styles.dot} ${live ? styles.dotLive : ''} ${styles.statusSlot}`}
              aria-hidden="true"
            />
          </li>
        );
      })}
    </ul>
  );
}
