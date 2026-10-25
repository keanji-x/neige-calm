import { GuideAnchor } from '../../../ui/floating-control/public.ts';
import { List, ListItem } from '@astryxdesign/core/List';
import { Icon } from '../../../ui/icon/public.tsx';
import type { TrackTemplate } from '../../../../../core/domain/track.ts';
import styles from './template-choices.module.css';

/** The roster owns purpose and prerequisites; choosing uses the same form state as the picker. */
export function TemplateChoices({ templates, disabled, onSelect }: Readonly<{
  templates: readonly TrackTemplate[];
  disabled: boolean;
  onSelect: (templateId: string) => void;
}>) {
  return <GuideAnchor id="template-choices" disabled={disabled}>{(anchor, describedBy) => <section ref={anchor} aria-describedby={describedBy} tabIndex={-1} className={styles.choices} aria-label="Choose a working method">
    <h2 className={styles.title}>Choose a working method</h2>
    <List density="compact">
      {templates.map((template) => <ListItem key={template.id}
        label={template.title}
        description={<>
          {template.description !== null && <span className={styles.description}>{template.description}</span>}
          {template.availability.status === 'requires_plugin'
            && <span className={styles.description}>Requires {template.availability.plugin_name}</span>}
        </>}
        endContent={<Icon name="chevron-right" size="sm" />}
        isDisabled={disabled}
        onClick={() => onSelect(template.id)} />)}
    </List>
  </section>}</GuideAnchor>;
}
