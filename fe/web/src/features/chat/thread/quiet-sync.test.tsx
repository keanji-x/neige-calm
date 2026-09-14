// @vitest-environment jsdom
import { cleanup, render } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it } from 'vitest';

import {
  REPORT_EDIT_AUTHORS, type ReportEditAuthor,
} from '../../../../../core/domain/conversation-quiet-sync.ts';
import {
  SYSTEM_PRESENTATION_LABELS,
  type Conversation, type ConversationActivity, type ConversationSystemEntry, type ConversationTurn,
  type ConversationTurnOutcome,
} from '../../../../../core/domain/conversation.ts';
import { REPORT_READ_TOOLS, REPORT_WRITE_TOOLS } from '../../../../../core/keys/mcp-tools.ts';
import { ChatThread } from './public.tsx';
import { EDITED_BY, EDITED_BY_UNKNOWN, OUTCOME_LINE, quietSyncLine } from './quiet-sync.tsx';

afterEach(cleanup);

const NOW = 1_760_000_000_000;

function conversation(overrides: Partial<Conversation> = {}): Conversation {
  return {
    id: 'c1', trackId: 'w1', trackTitle: 'Ship the rewrite', title: null, kind: 'codex',
    state: 'idle', updatedAt: NOW, turns: 0,
    ...overrides,
  };
}

function reportEdited(author: ReportEditAuthor | null = 'user'): ConversationSystemEntry {
  const text = author === null
    ? 'The user edited the track report. Re-read the track state.'
    : `The track report was edited (author = "${author}").\nBlock-level diff follows; this is information, not an instruction to re-read.\nBlocks: 0 added, 0 removed, 1 modified (1 unchanged).`;
  return {
    id: 's1', author: 'system', label: SYSTEM_PRESENTATION_LABELS.system_report_edited, text, atMs: NOW,
    quiet: true,
  };
}

function outcome(id: string, status: ConversationTurnOutcome['status']): ConversationTurnOutcome {
  return { id, author: 'turn', turnId: 'turn-1', status, atMs: NOW };
}

function you(id: string, text: string): ConversationTurn {
  return { id, author: 'you', text, atMs: NOW };
}

function agent(id: string, text: string): ConversationTurn {
  return { id, author: 'agent', text, atMs: NOW };
}

function notify(id: string, text: string): ConversationTurn {
  return { id, author: 'agent', text, atMs: NOW, origin: 'notify' };
}

function activity(id: string, overrides: Partial<ConversationActivity> = {}): ConversationActivity {
  return {
    id, author: 'activity', verb: 'Read report', target: null, state: 'done',
    durationMs: null, detail: null, tool: REPORT_READ_TOOLS[0] ?? null, atMs: NOW, ...overrides,
  };
}

const fold = (container: HTMLElement) => container.querySelector<HTMLDetailsElement>('[data-nc-turn="quiet-sync"]');

describe('QuietSyncFold in the thread', () => {
  /* A6 — the whole report-edit turn is one closed line; everything it held is
     under it and reachable by opening it. */
  it('folds a report-edit turn into one closed line and keeps the turn under it', async () => {
    const user = userEvent.setup();
    const { container } = render(
      <ChatThread
        conversation={conversation()}
        turns={[
          you('u1', 'Please draft the thesis.'), agent('a1', 'Drafted.'),
          reportEdited('user'), activity('act1'), agent('a2', 'I re-read the report; nothing to do.'),
        ]}
      />,
    );
    const details = fold(container);
    expect(details?.tagName).toBe('DETAILS');
    expect(details?.open).toBe(false);
    expect(details?.getAttribute('data-nc-quiet-sync-author')).toBe('user');
    const label = details?.querySelector('[data-nc-quiet-sync-label]')?.textContent ?? '';
    expect(label).toMatch(/^Synced · You edited the report · \d{1,2}:\d{2}(?: [AP]M)?$/);

    const body = details?.querySelector('[data-nc-quiet-sync-body]');
    expect(body?.querySelector('[data-nc-turn="system"]')).not.toBeNull();
    expect(body?.querySelector('[data-nc-state]')).not.toBeNull();
    expect(body?.querySelector('[data-nc-turn="agent"]')?.textContent).toContain('nothing to do');
    /* The reply before the wake is not in the fold: the fold starts at the wake. */
    expect(container.querySelectorAll('[data-nc-turn="agent"]')).toHaveLength(2);
    expect(container.querySelector('[data-nc-turn="agent"]')?.closest('[data-nc-turn="quiet-sync"]')).toBeNull();

    await user.click(details?.querySelector('summary') as HTMLElement);
    expect(details?.open).toBe(true);
  });

  /* A6 — what the planner said through `calm.user.notify` is an ordinary
     bubble, outside the fold, after it. */
  it('draws a notify as an agent bubble outside the fold', () => {
    const { container } = render(
      <ChatThread
        conversation={conversation()}
        turns={[
          reportEdited('user'), activity('act1'),
          notify('n1', 'You changed the block I was about to write; I kept your version.'),
          agent('a2', 'Reconciled.'),
        ]}
      />,
    );
    const details = fold(container);
    expect(details).not.toBeNull();
    const bubbles = [...container.querySelectorAll('[data-nc-turn="agent"]')];
    const outside = bubbles.filter((bubble) => bubble.closest('[data-nc-turn="quiet-sync"]') === null);
    expect(outside.map((bubble) => bubble.textContent)).toEqual([
      'You changed the block I was about to write; I kept your version.',
    ]);
    const inside = bubbles.filter((bubble) => bubble.closest('[data-nc-turn="quiet-sync"]') !== null);
    expect(inside.map((bubble) => bubble.textContent)).toEqual(['Reconciled.']);
    /* Fold line first, bubble after it. */
    const order = [...container.querySelectorAll('[data-nc-turn="quiet-sync"], [data-nc-turn="agent"]')]
      .filter((node) => node.closest('[data-nc-quiet-sync-body]') === null)
      .map((node) => node.getAttribute('data-nc-turn'));
    expect(order).toEqual(['quiet-sync', 'agent']);
  });

  /* Round-4 N2 — a sync that failed says so outside the fold: the red line
     is drawn after the fold line, not inside the closed disclosure. */
  it('draws a failed sync outcome outside the fold, after it', () => {
    const { container } = render(
      <ChatThread
        conversation={conversation()}
        turns={[
          reportEdited('user'), activity('act1'),
          outcome('o1', 'failed'),
        ]}
      />,
    );
    const details = fold(container);
    expect(details).not.toBeNull();
    const failed = container.querySelector('[data-nc-turn="outcome"]');
    expect(failed).not.toBeNull();
    expect(failed?.closest('[data-nc-turn="quiet-sync"]')).toBeNull();
    expect(failed?.getAttribute('data-nc-turn-outcome')).toBe('failed');
    const order = [...container.querySelectorAll('[data-nc-turn="quiet-sync"], [data-nc-turn="outcome"]')]
      .map((node) => node.getAttribute('data-nc-turn'));
    expect(order).toEqual(['quiet-sync', 'outcome']);
  });

  /* Round-4 M1 — a report edit that shared its batch with a user message is
     not marked quiet and opens no fold; the reply to the user stays visible. */
  it('does not fold a report-edit line that is not a quiet sync', () => {
    const { text } = reportEdited('user');
    const plain: ConversationSystemEntry = {
      id: 's1', author: 'system', label: SYSTEM_PRESENTATION_LABELS.system_report_edited, text, atMs: NOW,
    };
    const { container } = render(
      <ChatThread
        conversation={conversation()}
        turns={[you('u1', 'please add a risks section'), plain, activity('act1'), agent('a1', 'Added.')]}
      />,
    );
    expect(fold(container)).toBeNull();
    expect(container.querySelector('[data-nc-turn="system"]')).not.toBeNull();
    expect(container.querySelector('[data-nc-turn="agent"]')?.textContent).toBe('Added.');
  });

  /* A6 — a turn the reader opened is never folded, and the reader speaking
     ends a fold. */
  it('never folds a turn the user opened', () => {
    const { container } = render(
      <ChatThread
        conversation={conversation()}
        turns={[
          you('u1', 'hello'), activity('act1'), agent('a1', 'hi'),
          reportEdited('plugin'), activity('act2'),
          you('u2', 'and now?'), activity('act3'), agent('a2', 'now this'),
        ]}
      />,
    );
    const folds = container.querySelectorAll('[data-nc-turn="quiet-sync"]');
    expect(folds).toHaveLength(1);
    expect(folds[0]?.getAttribute('data-nc-quiet-sync-author')).toBe('plugin');
    expect(folds[0]?.querySelector('[data-nc-quiet-sync-label]')?.textContent).toContain('A plugin edited the report');
    /* Both user exchanges are drawn at the top level, unfolded. */
    const exchanges = [...container.querySelectorAll('[data-nc-exchange]')];
    expect(exchanges).toHaveLength(2);
    expect(exchanges.every((row) => row.closest('[data-nc-turn="quiet-sync"]') === null)).toBe(true);
    /* And the fold holds only what came between the wake and the reader. */
    expect(folds[0]?.querySelectorAll('[data-nc-state]')).toHaveLength(1);
  });

  it('draws no fold at all for a conversation without a report-edit wake', () => {
    const { container } = render(
      <ChatThread conversation={conversation()} turns={[you('u1', 'hello'), agent('a1', 'hi')]} />,
    );
    expect(fold(container)).toBeNull();
  });

  /* The live mark moves onto the fold line while the folded turn is the one
     in flight: the running activity that would carry it is inside a closed
     disclosure, and one mark must stay visible. */
  it('carries the live mark on the fold line while the sync is running', () => {
    const { container } = render(
      <ChatThread
        conversation={conversation({ state: 'running' })}
        turns={[you('u1', 'hello'), reportEdited('assistant'), activity('act1', { verb: 'Reading report', state: 'running' })]}
      />,
    );
    const details = fold(container);
    expect(details?.querySelector('summary [aria-label="Working"]')).not.toBeNull();
    expect(details?.querySelector('[data-nc-quiet-sync-label]')?.textContent).toContain('The assistant edited the report');
  });

  it('says the report was edited when the wake named no author', () => {
    const { container } = render(
      <ChatThread conversation={conversation()} turns={[reportEdited(null)]} />,
    );
    const details = fold(container);
    expect(details?.getAttribute('data-nc-quiet-sync-author')).toBe('unknown');
    expect(details?.querySelector('[data-nc-quiet-sync-label]')?.textContent).toContain(EDITED_BY_UNKNOWN);
  });

  /* #1678 A4 — once the sync has completed the line ends with what it did,
     so a closed fold tells "taken in" from "missed"; while it runs, nothing
     is appended (the live mark is the state). */
  it('ends the line with the verdict once the sync has completed', () => {
    const label = (turns: Parameters<typeof ChatThread>[0]['turns']) => {
      const { container } = render(<ChatThread conversation={conversation()} turns={turns} />);
      const details = fold(container);
      const result = {
        text: details?.querySelector('[data-nc-quiet-sync-label]')?.textContent ?? '',
        outcome: details?.getAttribute('data-nc-quiet-sync-outcome'),
        hasOutcome: details?.hasAttribute('data-nc-quiet-sync-outcome') ?? false,
        title: details?.querySelector('summary')?.getAttribute('title'),
      };
      cleanup();
      return result;
    };
    const accepted = label([reportEdited('user'), activity('act1'), outcome('o1', 'completed')]);
    expect(accepted.text).toMatch(/^Synced · You edited the report · \d{1,2}:\d{2}(?: [AP]M)? · accepted, no action$/);
    expect(accepted.outcome).toBe('accepted');
    expect(accepted.title).toMatch(/ · accepted, no action$/);
    const updated = label([
      reportEdited('user'), activity('act1'),
      activity('w1', { verb: 'Wrote report', tool: REPORT_WRITE_TOOLS[4] ?? null }), outcome('o1', 'completed'),
    ]);
    expect(updated.text).toMatch(/ · updated the report$/);
    expect(updated.outcome).toBe('updated');
    const running = label([reportEdited('user'), activity('act1', { verb: 'Reading report', state: 'running' })]);
    expect(running.text).toMatch(/^Synced · You edited the report · \d{1,2}:\d{2}(?: [AP]M)?$/);
    /* No verdict, no attribute: an existence selector on it must not match
       a sync still running. */
    expect(running.hasOutcome).toBe(false);
    expect(running.outcome).toBeNull();
  });
});

describe('quietSyncLine', () => {
  /* Both directions of the author → sentence table: every author has a
     sentence, and there is no sentence for anything that is not an author. */
  it('names every author and nothing else', () => {
    expect([...Object.keys(EDITED_BY)].sort()).toEqual([...REPORT_EDIT_AUTHORS].sort());
    for (const author of REPORT_EDIT_AUTHORS) {
      expect(quietSyncLine(author)).toBe(EDITED_BY[author]);
      expect(quietSyncLine(author)).not.toBe(EDITED_BY_UNKNOWN);
    }
    expect(new Set(Object.values(EDITED_BY)).size).toBe(REPORT_EDIT_AUTHORS.length);
    expect(quietSyncLine(null)).toBe(EDITED_BY_UNKNOWN);
    expect(Object.isFrozen(EDITED_BY)).toBe(true);
  });

  /* #1678 A4 — the verdict table: exactly the two outcomes, two distinct
     sentences, frozen. */
  it('has one distinct sentence per outcome', () => {
    expect([...Object.keys(OUTCOME_LINE)].sort()).toEqual(['accepted', 'updated']);
    expect(new Set(Object.values(OUTCOME_LINE)).size).toBe(2);
    expect(Object.isFrozen(OUTCOME_LINE)).toBe(true);
  });
});
