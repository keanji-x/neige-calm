import { Icon } from '@astryxdesign/core/Icon';
import { type RefCallback } from 'react';
import type { Area } from '../../../../core/domain/area.ts';
import { MobileHeader } from '../../ui/mobile-header/public.tsx';
import { Menu } from '../../ui/menu/public.tsx';
import { AreaSelector } from './area-selector.tsx';
import styles from './mobile-header.module.css';

export function MobileWorkspaceHeader({
  areas, activeArea, navigationOpen, onOpenNavigation, onSelectArea, onCreateArea,
  actionsHostRef, titleHostRef, hasTrack,
}: Readonly<{
  areas: readonly Area[];
  activeArea: Area | undefined;
  navigationOpen: boolean;
  onOpenNavigation: () => void;
  onSelectArea: (areaId: string) => void;
  onCreateArea: () => void;
  actionsHostRef: RefCallback<HTMLDivElement>;
  titleHostRef: RefCallback<HTMLDivElement>;
  hasTrack: boolean;
}>) {
  return <div data-nc-workspace-header="" inert={navigationOpen} aria-hidden={navigationOpen || undefined}>
    <MobileHeader title={hasTrack ? 'Track' : activeArea?.name ?? 'Choose area'}
      titleContent={hasTrack ? <div ref={titleHostRef} className={styles.trackTitleHost} /> :
        <AreaSelector areas={areas} activeArea={activeArea} onSelectArea={onSelectArea} onCreateArea={onCreateArea} />}
      leading={<button type="button" className={styles.iconButton} aria-label="Open areas"
        aria-expanded={navigationOpen} aria-controls="mobile-workspace-navigation" onClick={onOpenNavigation}>
        <Icon icon="menu" size="md" color="inherit" />
      </button>}
      actions={
    <div ref={actionsHostRef} className={styles.tools}>
      {!hasTrack && <Menu wrapClassName={styles.toolsMenuWrap} menuClassName={styles.toolsMenu} itemClassName={styles.menuItem}
        items={[
          { label: 'Cards', disabled: true, onSelect: () => undefined },
          { label: 'Conversations', disabled: true, onSelect: () => undefined },
        ]}
        trigger={(props) => <button {...props} type="button" className={styles.iconButton} aria-label="Track actions">
          <Icon icon="moreHorizontal" size="md" color="inherit" />
        </button>}
      />}
    </div>}
    />
  </div>;
}
