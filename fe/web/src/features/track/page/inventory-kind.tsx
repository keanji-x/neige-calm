import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';
import { Icon, type IconName } from '../../../ui/icon/public.tsx';

/** The Track inventory owns this closed graphical vocabulary. Unknown kinds retain their exact name. */
const KIND_ICONS: Readonly<Record<string, IconName>> = Object.freeze({
  codex: 'codex', claude: 'claude', terminal: 'terminal', opencode: 'agent', grok: 'agent',
});
/** The same type vocabulary for entry icons and accessible worker controls. */
export function InventoryIcon({ kind }: { kind: string }) {
  return <Icon name={Object.hasOwn(KIND_ICONS, kind) ? KIND_ICONS[kind] : 'file'} size="sm" />;
}
/** Exact text remains in the field and accessible name; only its visual presentation is an icon. */
export function InventoryKind({ kind }: { kind: string }) {
  return <><InventoryIcon kind={kind} /><VisuallyHidden>{kind}</VisuallyHidden></>;
}
