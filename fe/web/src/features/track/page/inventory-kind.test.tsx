import { render } from '@testing-library/react';
import { expect, it } from 'vitest';
import type { RowModuleView } from '../../../../../core/view/panel.ts';
import { checkProjection } from '../../../../../tools/projection/public.ts';
import { openInventoryDisclosures } from './test-fixtures.tsx';
import { makeDesktopPainter, type DesktopLeaf } from './desktop-painter.tsx';

it('preserves exact kind fields and accessible worker names when their text becomes an icon', () => {
  const module: RowModuleView = { key: 'tasks', title: 'Tasks', empty: '还没有任务', rows:
    ['codex', 'claude', 'future-provider', 'constructor', 'toString', '__proto__'].map(kind => ({
      id: kind, title: `${kind}-task`, kind, status: null, activity: null, badges: [],
      actions: [{ kind: 'open-card' as const, cardId: kind, label: null, hint: `Open ${kind}`, description: null }],
    })) };
  const mount = (leaves: readonly DesktopLeaf[]) => { const root = render(<>{leaves.map(leaf => {
    if (leaf.slot !== 'module') throw new Error('Expected a module');
    return leaf.node;
  })}</>).container; openInventoryDisclosures(root); return root; };
  expect(checkProjection(makeDesktopPainter({}), [module], mount)).toEqual([]);
});
