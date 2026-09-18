import { cleanup, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import '../../../styles/entry.css';
import { ChatThread } from './public.tsx';
import { SYSTEM_PRESENTATION_LABELS,
  type Conversation, type ConversationActivity, type ConversationSystemEntry,
  type ConversationTurnOutcome,
} from '../../../../../core/domain/conversation.ts';

afterEach(cleanup);

/* The thread's working marks: `ui/activity-indicator`, decorative by contract
   (#1722 §6, the S2 a11y contract) — the accessible "in motion" fact is said
   once, by the pending-reply placeholder's hidden text, and asserted by name
   where it matters. Counted by the marker, not by a label. */
const workingMarks = () => document.querySelectorAll('[data-nc-activity="working"]');

const conversation: Conversation = {
  id: 'integration', trackId: 'w1', trackTitle: 'Tools', title: null,
  kind: 'codex', state: 'idle', updatedAt: 1, turns: 0,
};
const wake: ConversationSystemEntry = {
  id: 'wake', author: 'system', label: SYSTEM_PRESENTATION_LABELS.system_report_edited,
  text: 'The track report was edited (author = "user").', quiet: true, atMs: 2,
};
function activity(id: string, state: ConversationActivity['state'] = 'done'): ConversationActivity {
  return { id, author: 'activity', tool: null, verb: 'Ran', target: id,
    state, durationMs: null, detail: null, atMs: 1 };
}
function outcome(status: ConversationTurnOutcome['status']): ConversationTurnOutcome {
  return { id: 'outcome', author: 'turn', turnId: 't1', status, atMs: 3 };
}

describe('tool groups alongside quiet syncs and turn outcomes', () => {
  it('keeps quiet tools inside their own disclosure without joining ordinary groups across it', () => {
    const { container } = render(<ChatThread cards={{}} conversation={conversation} turns={[
      activity('before-1'), activity('before-2'), wake,
      activity('quiet-1'), activity('quiet-2'), outcome('completed'),
      { id: 'you', author: 'you', text: 'Continue', atMs: 4 },
      activity('after-1'), activity('after-2'),
    ]} />);
    const groups = container.querySelectorAll<HTMLElement>('[aria-expanded]');
    expect(groups).toHaveLength(2);
    expect(groups[0]?.textContent).toContain('before-2');
    expect(groups[1]?.textContent).toContain('after-2');
    const fold = container.querySelector<HTMLDetailsElement>('[data-nc-turn="quiet-sync"]')!;
    const quiet = screen.getByText('quiet-1');
    expect(fold.open).toBe(false);
    expect(fold.contains(quiet)).toBe(true);
    expect(quiet.checkVisibility()).toBe(false);
    fireEvent.click(fold.querySelector('summary')!);
    expect(fold.open).toBe(true);
    expect(quiet.checkVisibility()).toBe(true);
    expect(fold.querySelector('[aria-expanded]')).toBeNull();
    expect(container.querySelector('[data-nc-turn="outcome"]')).toBeNull();
  });

  it('keeps one live mark on the quiet summary even when its running tools are revealed', () => {
    const { container } = render(<ChatThread cards={{}} conversation={conversation} pending turns={[
      wake, activity('quiet-done'), activity('quiet-running', 'running'),
    ]} />);
    const fold = container.querySelector<HTMLDetailsElement>('[data-nc-turn="quiet-sync"]')!;
    expect(workingMarks()).toHaveLength(1);
    expect(fold.querySelector('summary [data-nc-activity="working"]')).not.toBeNull();
    fireEvent.click(fold.querySelector('summary')!);
    expect(workingMarks()).toHaveLength(1);
    expect(screen.queryByRole('status', { name: 'Loading' })).toBeNull();
  });

  it.each(['completed', 'failed', 'interrupted'] as const)('treats a %s outcome as a tool-run boundary', (status) => {
    const { container } = render(<ChatThread cards={{}} conversation={conversation} turns={[
      activity('before-1'), activity('before-2'), outcome(status), activity('after-1'), activity('after-2'),
    ]} />);
    expect(container.querySelectorAll('[aria-expanded]')).toHaveLength(2);
    expect(container.querySelector('[data-nc-turn="outcome"]')?.getAttribute('data-nc-turn-outcome') ?? null)
      .toBe(status === 'completed' ? null : status);
  });
});
