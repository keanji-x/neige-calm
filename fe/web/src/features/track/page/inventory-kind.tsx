import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';
import type { InventoryGroupKey } from '../../../../../core/view/panel-groups.ts';
import { Icon, type IconName } from '../../../ui/icon/public.tsx';

/** The Track inventory owns this closed graphical vocabulary. Unknown kinds retain their exact name. */
const KIND_ICONS: Readonly<Record<string, IconName>> = Object.freeze({
  codex: 'codex', claude: 'claude', terminal: 'terminal', opencode: 'agent',
});
const GROUP_ICONS: Readonly<Record<InventoryGroupKey, IconName>> = Object.freeze({
  terminals: 'terminal', agents: 'agent', 'other-tools': 'tools', working: 'status-running',
  attention: 'notification', waiting: 'status-waiting', failed: 'status-failed',
  done: 'status-done', canceled: 'status-exited', other: 'file',
});

export function inventoryGroupIcon(key: InventoryGroupKey): IconName {
  return GROUP_ICONS[key];
}

/** Exact text remains in the field and accessible name; only its visual presentation is an icon. */
export function InventoryKind({ kind }: { kind: string }) {
  return <><Icon name={Object.hasOwn(KIND_ICONS, kind) ? KIND_ICONS[kind] : 'file'} size="sm" /><VisuallyHidden>{kind}</VisuallyHidden></>;
}
