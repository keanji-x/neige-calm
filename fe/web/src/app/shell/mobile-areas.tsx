// The Areas sheet: the area list, and one area's tracks.
//
// Presentational. The drill-in used to live here as `selectedAreaId` plus a
// coupled `motion`, and the shell learned about it through a `window`
// CustomEvent — three owners for one transition. Both now sit in `app/shell`
// as a single state (#1191 §2.2), because the shell is what has to derive
// "a secondary page is showing" from it and what has to restore it when the
// reader returns from a report with `?from=area`.
//
// The unmount cleanup that used to publish "secondary closed" is gone with the
// event: the shell's formula conjoins `mobileSection === 'areas'`, so a sheet
// that is not rendered cannot claim the screen.

import { IconButton } from '@astryxdesign/core/IconButton';
import type { Area } from '../../../../core/domain/area.ts';
import { visibleAreas } from '../../../../core/domain/area.ts';
import {
  lifecycleLabel, visibleTracks, trackDisplayTitle, type Track,
} from '../../../../core/domain/track.ts';
import {
  MobileList, MobileListEmpty, MobileListItem, MobileListPage,
} from '../../ui/mobile-list/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';

export function MobileAreas({
  areas, tracksByArea, selectedAreaId, motion,
  onSelectArea, onBack, onCreateArea, onEditArea, onOpenTrack,
}: Readonly<{
  areas: readonly Area[];
  tracksByArea: ReadonlyMap<string, readonly Track[]>;
  selectedAreaId: string | null;
  motion: 'none' | 'forward' | 'back';
  onSelectArea: (areaId: string) => void;
  onBack: () => void;
  onCreateArea: () => void;
  onEditArea: (area: Area) => void;
  onOpenTrack: (trackId: string) => void;
}>) {
  const rows = visibleAreas(areas);
  const selected = selectedAreaId === null ? undefined : rows.find((area) => area.id === selectedAreaId);

  if (selected !== undefined) {
    const tracks = visibleTracks(tracksByArea.get(selected.id) ?? []);
    return (
      <MobileListPage
        title={selected.name}
        backLabel="Areas"
        motion={motion}
        onBack={onBack}
        actions={(
          <IconButton
            label={`Edit area ${selected.name}`}
            icon={<Icon name="more" />}
            variant="ghost"
            size="lg"
            onClick={() => onEditArea(selected)}
          />
        )}
      >
        <MobileList title="Tracks">
          {tracks.map((track) => (
            <MobileListItem
              key={track.id}
              title={trackDisplayTitle(track.title)}
              meta={lifecycleLabel(track.lifecycle)}
              onSelect={() => onOpenTrack(track.id)}
            />
          ))}
          {tracks.length === 0 && <MobileListEmpty>No tracks in this area yet.</MobileListEmpty>}
        </MobileList>
      </MobileListPage>
    );
  }

  return (
    <MobileListPage
      title="Areas"
      motion={motion}
      actions={(
        <IconButton
          label="New area"
          icon={<Icon name="plus" />}
          variant="ghost"
          size="lg"
          onClick={onCreateArea}
        />
      )}
    >
      <MobileList>
        {rows.map((area) => {
          const tracks = visibleTracks(tracksByArea.get(area.id) ?? []);
          return (
            <MobileListItem
              key={area.id}
              title={area.name}
              meta={`${tracks.length} ${tracks.length === 1 ? 'track' : 'tracks'}`}
              onSelect={() => onSelectArea(area.id)}
            />
          );
        })}
      </MobileList>
    </MobileListPage>
  );
}
