import {
  SegmentedControl as AstryxSegmentedControl,
  SegmentedControlItem as AstryxSegmentedControlItem,
} from '@astryxdesign/core/SegmentedControl';

import { coveOf, visibleCoves, type Cove } from '../../../../core/domain/cove.ts';
import {
  visibleWaves, waveDisplayTitle, type Wave,
} from '../../../../core/domain/wave.ts';
import {
  MobileList, MobileListEmpty, MobileListItem, MobileListPage,
} from '../../ui/mobile-list/public.tsx';
import { useState } from '../../ui/state/public.ts';
import styles from './mobile-pages.module.css';

const RECENT_PAGE_LIMIT = 24;

export function MobilePages({ coves, waves, onOpenWave }: Readonly<{
  coves: readonly Cove[];
  waves: readonly Wave[];
  onOpenWave: (waveId: string) => void;
}>) {
  const visible = visibleWaves(waves);
  const shownCoves = visibleCoves(coves);
  const pinned = visible
    .filter((wave) => wave.pinnedAt !== null)
    .toSorted((left, right) => (right.pinnedAt ?? 0) - (left.pinnedAt ?? 0));
  const recent = visible
    .filter((wave) => wave.pinnedAt === null)
    .toSorted((left, right) => right.updatedAt - left.updatedAt)
    .slice(0, RECENT_PAGE_LIMIT);
  const [group, setGroup] = useState<'pinned' | 'recent'>(() => (pinned.length > 0 ? 'pinned' : 'recent'));
  const shown = group === 'pinned' ? pinned : recent;
  const coveFor = (wave: Wave) => coveOf(wave.coveId, shownCoves);

  return (
    <MobileListPage title="Pages">
      <AstryxSegmentedControl
        className={styles.groups}
        value={group}
        onChange={(value) => setGroup(value === 'pinned' ? 'pinned' : 'recent')}
        label="Page group"
        size="sm"
      >
        <AstryxSegmentedControlItem value="pinned" label="Pinned" />
        <AstryxSegmentedControlItem value="recent" label="Recent" />
      </AstryxSegmentedControl>
      <MobileList>
        {shown.map((wave) => {
          const cove = coveFor(wave);
          return (
            <MobileListItem
              key={wave.id}
              title={waveDisplayTitle(wave.title)}
              meta={cove?.name ?? 'Unknown cove'}
              startContent={(
                <span
                  className={styles.coveDot}
                  data-nc-page-cove=""
                  style={cove === undefined ? undefined : { backgroundColor: cove.color }}
                  aria-hidden="true"
                />
              )}
              onSelect={() => onOpenWave(wave.id)}
            />
          );
        })}
        {shown.length === 0 && (
          <MobileListEmpty>{group === 'pinned' ? 'No pinned Pages.' : 'No recent Pages.'}</MobileListEmpty>
        )}
      </MobileList>
    </MobileListPage>
  );
}
