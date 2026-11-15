import { VisuallyHidden } from '@astryxdesign/core/VisuallyHidden';
import { agentProviderSchema } from '../../../../../core/api/schemas.ts';
import { providerPresentation } from '../../../../../core/domain/provider-presentation.ts';
import { Icon, type IconName } from '../../../ui/icon/public.tsx';

/** The Track inventory owns this closed graphical vocabulary. Unknown kinds retain their exact name. */
const KIND_ICONS: Readonly<Record<string, IconName>> = Object.freeze({
  terminal: 'terminal',
});
/** The same type vocabulary for entry icons and accessible worker controls. */
export function InventoryIcon({ kind }: { kind: string }) {
  const provider = agentProviderSchema.safeParse(kind);
  const presentation = providerPresentation(provider.success ? provider.data : null);
  return <Icon name={presentation?.icon ?? (Object.hasOwn(KIND_ICONS, kind) ? KIND_ICONS[kind] : 'file')} size="sm" />;
}
/** Exact text remains in the field and accessible name; only its visual presentation is an icon. */
export function InventoryKind({ kind }: { kind: string }) {
  return <><InventoryIcon kind={kind} /><VisuallyHidden>{kind}</VisuallyHidden></>;
}
