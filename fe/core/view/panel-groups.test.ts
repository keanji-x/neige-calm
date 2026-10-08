import { describe, expect, it } from 'vitest';
import type { PanelRow } from './panel.js';
import { groupPanelRows } from './panel-groups.js';

function row(id: string, status: string | null): PanelRow {
  return { id, title: id, kind: null, badges: [], status: status === null ? null : { token: status, phrase: status, detail: null }, activity: null, actions: [] };
}

describe('inventory status groups', () => {
  it('keeps every task in a stable group and expands work in progress and work needing attention', () => {
    const groups = groupPanelRows([row('done-a', 'done'), row('waiting', 'pending'), row('working', 'running'),
      row('done-b', 'done'), row('attention', 'needs_input'), row('failed', 'failed')], 'tasks');
    expect(groups.map(group => [group.key, group.rows.map(item => item.id), group.expanded])).toEqual([
      ['working', ['working'], true], ['attention', ['attention'], true],
      ['waiting', ['waiting'], false], ['failed', ['failed'], true], ['done', ['done-a', 'done-b'], false],
    ]);
  });

  it('keeps missing and unfamiliar tool states verbatim inside type groups', () => {
    const rows = [row('static', null), row('new-state', 'future-state'), row('idle', 'idle'), row('exited', 'exited')];
    const groups = groupPanelRows(rows, 'cards');
    expect(groups.map(group => [group.key, group.rows.map(item => item.id), group.attentionCount]))
      .toEqual([['other-tools', ['static', 'new-state', 'idle', 'exited'], 0]]);
    expect(groups[0].rows.map(item => item.status?.token ?? null)).toEqual([null, 'future-state', 'idle', 'exited']);
  });

  it('groups all terminals together and expands the type group when any item needs attention', () => {
    const rows = ['running', 'idle', 'failed', 'needs_input', 'future-state']
      .map(status => ({ ...row(status, status), kind: 'terminal' }));
    const groups = groupPanelRows(rows, 'cards');
    expect(groups.map(group => [group.key, group.expanded, group.attentionCount, group.rows.map(item => item.id)]))
      .toEqual([['terminals', true, 2, ['running', 'idle', 'failed', 'needs_input', 'future-state']]]);
    expect(groupPanelRows(rows.slice(0, 2), 'cards')[0].expanded).toBe(false);
  });

  it('groups registered providers as Agents independently of runtime state', () => {
    const rows = ['codex', 'claude', 'opencode'].map(kind => ({ ...row(kind, kind === 'codex' ? 'done' : 'exited'), kind }));
    const groups = groupPanelRows(rows, 'cards');
    expect(groups.map(group => [group.label, group.rows.map(item => item.status?.token)]))
      .toEqual([['Agents', ['done', 'exited', 'exited']]]);
    expect(groupPanelRows([rows[0]], 'tasks')[0].label).toBe('Completed');
  });

  it('preserves Tasks disclosure defaults when a worker activity needs attention', () => {
    const groups = groupPanelRows([{ ...row('done', 'done'), kind: 'codex', activity: 'failed' }], 'tasks');
    expect(groups[0].key).toBe('done');
    expect(groups[0].expanded).toBe(false);
    expect(groups[0].attentionCount).toBe(0);
  });

  it('counts explicit attention from kernel activity even when the session is still running', () => {
    const groups = groupPanelRows([{ ...row('terminal', 'running'), kind: 'terminal', activity: 'attention' }], 'cards');
    expect(groups[0].attentionCount).toBe(1);
    expect(groups[0].expanded).toBe(true);
  });
});
