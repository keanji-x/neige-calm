// One category roster drives desktop navigation, the mobile index and page titles.
import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { List, ListItem } from '@astryxdesign/core/List';
import { MobileListGroup } from '../../ui/mobile-list/public.tsx';
import styles from './settings.module.css';

export const SETTINGS_SECTIONS = Object.freeze([
  Object.freeze({ id: 'general', label: 'General', icon: 'menu' }),
  Object.freeze({ id: 'network', label: 'Network', icon: 'externalLink' }),
  Object.freeze({ id: 'appearance', label: 'Appearance', icon: 'viewColumns' }),
  Object.freeze({ id: 'plugins', label: 'Plugins', icon: 'wrench' }),
  Object.freeze({ id: 'planners', label: 'Planners', icon: 'success' }),
  Object.freeze({ id: 'about', label: 'About', icon: 'info' }),
] as const);

export type SettingsSection = typeof SETTINGS_SECTIONS[number]['id'];
export type SettingsPresentation = 'desktop' | 'mobile-index' | 'mobile-detail';

export function settingsSectionLabel(section: SettingsSection): string {
  // The type is derived from this roster, so a valid section always has a label.
  return SETTINGS_SECTIONS.find((entry) => entry.id === section)!.label;
}

export function SettingsIndex({ onSelectSection }: Readonly<{
  onSelectSection: (section: SettingsSection) => void;
}>) {
  return <nav aria-label="Settings categories">
    <MobileListGroup label="Settings">
      <List hasDividers={false} density="balanced" className={styles.categoryList}>
        {SETTINGS_SECTIONS.map((entry) => <ListItem key={entry.id} label={entry.label}
          className={styles.categoryRow}
          startContent={<AstryxIcon icon={entry.icon} size="md" color="secondary" />}
          endContent={<AstryxIcon icon="chevronRight" size="sm" color="secondary" />}
          onClick={() => onSelectSection(entry.id)} />)}
      </List>
    </MobileListGroup>
  </nav>;
}
