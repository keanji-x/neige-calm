import { floatingControlClassName } from '../../ui/floating-control/public.ts';
import { Icon } from '../../ui/icon/public.tsx';
import { type RefCallback } from 'react';
import type { Area } from '../../../../core/domain/area.ts';
import { MobileHeader } from '../../ui/mobile-header/public.tsx';
import { AreaSelector } from './area-selector.tsx';
import styles from './mobile-header.module.css';

export function MobileWorkspaceHeader({
  areas, activeArea, navigationOpen, onOpenNavigation, onSelectArea, onCreateArea,
  actionsHostRef, titleHostRef, onBack,
}: Readonly<{
  onBack?: () => void;
  areas: readonly Area[];
  activeArea: Area | undefined;
  navigationOpen: boolean;
  onOpenNavigation: () => void;
  onSelectArea: (areaId: string) => void;
  onCreateArea: () => void;
  actionsHostRef: RefCallback<HTMLDivElement>;
  titleHostRef: RefCallback<HTMLDivElement>;
}>) {
  return <div data-nc-workspace-header="" inert={navigationOpen} aria-hidden={navigationOpen || undefined}>
    <MobileHeader actionsHidden className={styles.rootHeader} title={activeArea?.name ?? 'Choose area'}
      titleContent={<div className={styles.titleContent}>
        <div ref={titleHostRef} className={styles.trackTitleHost} />
        <div className={styles.areaTitle}>
          <AreaSelector areas={areas} activeArea={activeArea} onSelectArea={onSelectArea} onCreateArea={onCreateArea} />
        </div>
      </div>}
      onBack={onBack} backLabel="Tracks"
      leading={onBack === undefined ? <button type="button" className={`${styles.iconButton} ${floatingControlClassName}`} aria-label="Open conversation history"
        aria-expanded={navigationOpen} aria-controls="mobile-conversation-history" onClick={onOpenNavigation}>
        <Icon name="menu" />
      </button> : undefined}
      actions={<div ref={actionsHostRef} className={styles.tools} hidden />}
    />
  </div>;
}
