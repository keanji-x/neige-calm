import type { RowModuleView } from '../../../../../core/view/panel.ts';
import { Icon } from '../../../ui/icon/public.tsx';
import type { EmptyStateSchema } from '../../../ui/empty-state/public.tsx';

/** Content for the inventory's closed set of module kinds. */
export function inventoryEmptyContent(key: RowModuleView['key'], title: string): EmptyStateSchema {
  switch (key) {
    case 'work':
    case 'tasks':
      return { icon: <Icon name="tasks" />, title, description: '报告中声明的任务会显示在这里。' };
    case 'temporary':
      return { icon: <Icon name="terminal" />, title, description: '临时终端会显示在这里。' };
    case 'cards':
      return { icon: <Icon name="tools" />, title, description: '添加的工具会显示在这里。' };
  }
}
