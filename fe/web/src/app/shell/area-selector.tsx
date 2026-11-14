import { useRef } from 'react';
import type { DialogOrigin } from '../../ui/dialog/public.tsx';
import { Button } from '@astryxdesign/core/Button';
import { Icon } from '../../ui/icon/public.tsx';
import { visibleAreas, type Area } from '../../../../core/domain/area.ts';
import { Menu } from '../../ui/menu/public.tsx';
import styles from './area-selector.module.css';

/** The caller owns whether selection navigates or just changes a list's scope. */
export function AreaSelector({ areas, activeArea, draftArea, onSelectArea, onCreateArea }: Readonly<{
  areas: readonly Area[];
  activeArea: Area | undefined;
  /** Presentation only; draft metadata never adds a selectable Area. */
  draftArea?: Pick<Area, 'id' | 'name'>;
  onSelectArea: (areaId: string) => void;
  onCreateArea: (origin: DialogOrigin) => void;
}>) {
  const sourceRef = useRef<HTMLSpanElement | null>(null);
  const shown = visibleAreas(areas);
  // A newly created Area can name the draft before the full picker list arrives.
  const current = shown.find((area) => area.id === activeArea?.id)
    ?? visibleAreas(activeArea === undefined ? [] : [activeArea])[0]
    ?? draftArea;
  return <Menu wrapClassName={styles.area} menuClassName={styles.menu}
    itemClassName={styles.item} separatorClassName={styles.separator}
    items={[
      ...shown.map((area) => ({ label: area.name, current: area.id === current?.id, onSelect: () => onSelectArea(area.id) })),
      { label: 'New area', separatorBefore: true, onSelect: () => onCreateArea({ kind: 'anchor', anchor: { ref: sourceRef }, unavailable: 'surface-center' }) },
    ]}
    trigger={(props) => <span ref={sourceRef} style={{ display: 'inline-flex' }}><Button {...props} label={current?.name ?? 'Choose area'}
      variant="ghost" size="lg" className={styles.button}
      aria-label={`Switch area, ${current?.name ?? 'Choose area'}`}
      endContent={<Icon name="switch" />} /></span>}
  />;
}
