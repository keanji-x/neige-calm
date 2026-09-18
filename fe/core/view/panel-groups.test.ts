import { describe, expect, it } from 'vitest';
import type { PanelRow } from './panel.js';
import { groupPanelRows } from './panel-groups.js';

function row(id: string, status: string | null): PanelRow {
  return { id, title: id, kind: null, badges: [], status: status === null ? null : { token: status, phrase: status }, activity: null, actions: [] };
}

describe('inventory status groups', () => {
  it('keeps every task in a stable group and expands only work in progress', () => {
    const groups = groupPanelRows([row('done-a', 'done'), row('waiting', 'pending'), row('working', 'running'),
      row('done-b', 'done'), row('attention', 'needs_input'), row('failed', 'failed')], 'tasks');
    expect(groups.map(group => [group.key, group.rows.map(item => item.id), group.expanded])).toEqual([
      ['working', ['working'], true], ['attention', ['attention'], false],
      ['waiting', ['waiting'], false], ['failed', ['failed'], false], ['done', ['done-a', 'done-b'], false],
    ]);
  });

  it('never calls missing or unfamiliar runtime states completed', () => {
    const groups = groupPanelRows([row('static', null), row('new-state', 'future-state'), row('idle', 'idle'), row('exited', 'exited')], 'cards');
    expect(groups.flatMap(group => group.rows)).toHaveLength(4);
    expect(groups.find(group => group.key === 'done')?.rows.map(item => item.id)).toEqual(['exited']);
    expect(groups.find(group => group.key === 'ready')?.rows.map(item => item.id)).toEqual(['static', 'idle']);
    expect(groups.find(group => group.key === 'other')?.rows.map(item => item.id)).toEqual(['new-state']);
  });
});
