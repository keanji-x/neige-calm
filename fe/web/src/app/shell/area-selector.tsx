import { Button } from '@astryxdesign/core/Button';
import { Icon } from '@astryxdesign/core/Icon';
import { visibleAreas, type Area } from '../../../../core/domain/area.ts';
import { Menu } from '../../ui/menu/public.tsx';
import styles from './area-selector.module.css';

/** The caller owns whether selection navigates or just changes a list's scope. */
export function AreaSelector({ areas, activeArea, onSelectArea, onCreateArea }: Readonly<{
  areas: readonly Area[];
  activeArea: Area | undefined;
  onSelectArea: (areaId: string) => void;
  onCreateArea: () => void;
}>) {
  const shown = visibleAreas(areas);
  const current = shown.find((area) => area.id === activeArea?.id);
  return <Menu wrapClassName={styles.area} menuClassName={styles.menu}
    itemClassName={styles.item} separatorClassName={styles.separator}
    items={[
      ...shown.map((area) => ({ label: area.name, current: area.id === current?.id, onSelect: () => onSelectArea(area.id) })),
      { label: 'New area', separatorBefore: true, onSelect: onCreateArea },
    ]}
    trigger={(props) => <Button {...props} label={current?.name ?? 'Choose area'}
      variant="ghost" size="lg" className={styles.button}
      aria-label={`Switch area, ${current?.name ?? 'Choose area'}`}
      endContent={<Icon icon="chevronDown" size="sm" color="inherit" />} />}
  />;
}
