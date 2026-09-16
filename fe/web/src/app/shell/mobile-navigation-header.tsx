import { MobileHeader } from '../../ui/mobile-header/public.tsx';
import { List, ListItem } from '@astryxdesign/core/List';
import { MobileListGroup } from '../../ui/mobile-list/public.tsx';
import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { IconButton } from '@astryxdesign/core/IconButton';
import { Menu } from '../../ui/menu/public.tsx';
import menuStyles from './mobile-header.module.css';
import type { Area } from '../../../../core/domain/area.ts';
import { Icon } from '../../ui/icon/public.tsx';
import styles from './mobile-navigation.module.css';

export type MobileNavigationActions = Readonly<{
  onBack: () => void;
  onNewTrack: (areaId: string) => void;
  onEditArea: (area: Area) => void;
  onOpenSettings: () => void;
  onCreateArea: () => void;
}>;

/** Keep navigation in the title bar and workspace actions in one stable group. */
export function MobileNavigationHeader({
  title, area, creationScope, backLabel, onBack, onNewTrack, onEditArea, onOpenSettings, onCreateArea,
}: Readonly<{ title: string; area: Area | undefined; creationScope: 'area' | 'track' | 'both'; backLabel: string }> & MobileNavigationActions) {
  return <>
    <MobileHeader title={title} backLabel={backLabel} onBack={onBack}
      actions={<Menu wrapClassName={menuStyles.toolsMenuWrap} menuClassName={menuStyles.toolsMenu} itemClassName={menuStyles.menuItem}
        items={[{ label: area === undefined ? 'Edit area' : `Edit area ${area.name}`, disabled: area === undefined,
          onSelect: () => { if (area !== undefined) onEditArea(area); } }]}
        trigger={(props) => <IconButton {...props} label="Area actions" icon={<AstryxIcon icon="moreHorizontal" size="md" color="inherit" />}
          variant="ghost" size="lg" className={styles.iconButton} />} />} />
    <div className={styles.actionRows}><MobileListGroup label="Workspace actions">
      <List density="balanced" className={styles.actionList}>
        <ListItem label="Settings" className={styles.actionRow}
          startContent={<AstryxIcon icon="wrench" size="md" color="secondary" />}
          onClick={onOpenSettings} />
        {creationScope !== 'area' && <ListItem label="New track" className={styles.actionRow}
          startContent={<Icon name="plus" />} isDisabled={area === undefined}
          onClick={() => { if (area !== undefined) onNewTrack(area.id); }} />}
        {creationScope !== 'track' && <ListItem label="New area" className={styles.actionRow}
          startContent={<Icon name="plus" />} onClick={onCreateArea} />}
      </List>
    </MobileListGroup></div>
  </>;
}
