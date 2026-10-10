import { describe, expect, it } from 'vitest';

import type { WorkerSessionState } from '../api/schemas.js';
import { claudeCardAction, restartClaudeCardOperation, type ClaudeCardAction } from './claude-card.js';

const STATES: readonly (WorkerSessionState | null)[] = Object.freeze([
  null, 'starting', 'running', 'idle', 'turn_pending', 'exited', 'failed', 'superseded',
]);

/** [running, installed]: same, differ, and each side unknown. */
const VERSIONS: readonly (readonly [string | null, string | null])[] = Object.freeze([
  ['2.1.280', '2.1.280'], ['2.1.259', '2.1.280'], [null, '2.1.280'], ['2.1.259', null], [null, null],
]);

/** The rule, written out once per cell rather than computed: a task worker's card never has an action. */
function expected(owner: boolean, state: WorkerSessionState | null, running: string | null, installed: string | null): ClaudeCardAction | null {
  if (!owner) return null;
  if (state === 'exited' || state === 'failed' || state === 'superseded') return 'resume';
  if (state === 'running' && running === '2.1.259' && installed === '2.1.280') return 'update';
  return null;
}

describe('claudeCardAction (#2516)', () => {
  it('offers Update or Resume only as the owner × state × version table says', () => {
    for (const owner of [true, false]) {
      for (const state of STATES) {
        for (const [running, installed] of VERSIONS) {
          const input = { ownerCreated: owner, sessionState: state, runningVersion: running, installedVersion: installed };
          expect(claudeCardAction(input), JSON.stringify(input)).toBe(expected(owner, state, running, installed));
        }
      }
    }
  });

  it('names the cases a person sees', () => {
    const owner = { ownerCreated: true, runningVersion: '2.1.259', installedVersion: '2.1.280' } as const;
    expect(claudeCardAction({ ...owner, sessionState: 'running' })).toBe('update');
    expect(claudeCardAction({ ...owner, sessionState: 'running', installedVersion: '2.1.259' })).toBeNull();
    expect(claudeCardAction({ ...owner, sessionState: 'exited' })).toBe('resume');
    expect(claudeCardAction({ ...owner, sessionState: 'exited', ownerCreated: false })).toBeNull();
    expect(claudeCardAction({ ...owner, sessionState: 'running', ownerCreated: false })).toBeNull();
  });

  it('restarts through the card route', () => {
    expect(restartClaudeCardOperation('c/1')).toMatchObject({ method: 'POST', path: '/api/cards/c%2F1/claude/restart' });
  });
});
