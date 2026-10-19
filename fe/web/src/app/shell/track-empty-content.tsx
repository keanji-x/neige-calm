import { Icon } from '../../ui/icon/public.tsx';
import type { EmptyStateSchema } from '../../ui/empty-state/public.tsx';
import type { SidebarTrackGroups } from './sidebar-track-groups.ts';

/** Shared shortcut wording for desktop and mobile; membership stays in its owner. */
export function trackEmptyContent(view: keyof SidebarTrackGroups): EmptyStateSchema {
  switch (view) {
    case 'pinned': return { icon: <Icon name="pin" />, title: '还没有已置顶的 Track。', description: '置顶的 Track 会显示在这里。' };
    case 'unread': return { icon: <Icon name="chat" />, title: '没有未读的 Track。', description: '有新进展的 Track 会显示在这里。' };
    case 'waiting': return { icon: <Icon name="status-waiting" />, title: '没有需要处理的 Track。', description: '需要你处理的 Track 会显示在这里。' };
    case 'running': return { icon: <Icon name="status-running" />, title: '没有运行中的 Track。', description: '运行中的 Track 会显示在这里。' };
  }
}
