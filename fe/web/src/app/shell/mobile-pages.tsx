import {
  SegmentedControl as AstryxSegmentedControl,
  SegmentedControlItem as AstryxSegmentedControlItem,
} from '@astryxdesign/core/SegmentedControl';

import { areaOf, visibleAreas, type Area } from '../../../../core/domain/area.ts';
import {
  userVisibleTracks, trackDisplayTitle, type Track,
} from '../../../../core/domain/track.ts';
import {
  MobileList, MobileListEmpty, MobileListItem, MobileListPage,
} from '../../ui/mobile-list/public.tsx';
import { useState } from '../../ui/state/public.ts';
import styles from './mobile-pages.module.css';

const RECENT_PAGE_LIMIT = 24;

export function MobilePages({ areas, tracks, onOpenTrack }: Readonly<{
  areas: readonly Area[];
  tracks: readonly Track[];
  onOpenTrack: (trackId: string) => void;
}>) {
  /*
   * E2E-INV-SHELL-003 — the same second layer of defence the sidebar applies:
   * a track whose area is not user-visible does not belong on a list a person
   * reads, and filtering tracks alone (what this list used to do) let the
   * kernel's system area through if an unfiltered list ever reached here.
   */
  const visible = userVisibleTracks(tracks, areas);
  const shownAreas = visibleAreas(areas);
  const pinned = visible
    .filter((track) => track.pinnedAt !== null)
    .toSorted((left, right) => (right.pinnedAt ?? 0) - (left.pinnedAt ?? 0));
  const recent = visible
    .filter((track) => track.pinnedAt === null)
    .toSorted((left, right) => right.updatedAt - left.updatedAt)
    .slice(0, RECENT_PAGE_LIMIT);
  const [group, setGroup] = useState<'pinned' | 'recent'>(() => (pinned.length > 0 ? 'pinned' : 'recent'));
  const shown = group === 'pinned' ? pinned : recent;
  const areaFor = (track: Track) => areaOf(track.areaId, shownAreas);

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
        {shown.map((track) => {
          const area = areaFor(track);
          return (
            <MobileListItem
              key={track.id}
              title={trackDisplayTitle(track.title)}
              titleVariant="document"
              meta={area?.name ?? 'Unknown area'}
              startContent={(
                <span
                  className={styles.areaInitial}
                  data-nc-page-area=""
                  style={area === undefined ? undefined : { borderColor: area.color, color: area.color }}
                  aria-hidden="true"
                >
                  {area?.name.trim().charAt(0).toLocaleUpperCase() || '?'}
                </span>
              )}
              onSelect={() => onOpenTrack(track.id)}
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
