import { Badge } from '@astryxdesign/core/Badge';
import { Button } from '@astryxdesign/core/Button';
import type { TemplatePluginGuide } from '../../../../../core/domain/template.ts';
import { Icon } from '../../../ui/icon/public.tsx';
import styles from './template-plugin-guides.module.css';

export function TemplatePluginGuides({ guides, error, onRetry }: Readonly<{
  guides: readonly TemplatePluginGuide[] | undefined;
  /** What a failed read of the guides says, by the read rule; `null` when it did not fail. */
  error: string | null;
  onRetry: () => void;
}>) {
  if (error !== null) return <span role="alert">{error} <Button label="Retry" variant="ghost" size="sm" onClick={onRetry} /></span>;
  if (guides === undefined || guides.length === 0) return null;
  return <div className={styles.guides} role="group" aria-label="Template plugin guides">
    {guides.map(guide => <Badge key={guide.id} variant="neutral" label={guide.name}
      aria-label={`${guide.name}, included by template`}
      icon={<Icon name="lock" size="sm" />} />)}
  </div>;
}
