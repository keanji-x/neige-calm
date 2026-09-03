// The Areas sheet: the area list, and one area's waves.
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

import type { Area } from '../../../../core/domain/area.ts';
import { visibleAreas } from '../../../../core/domain/area.ts';
import {
  lifecycleLabel, visibleWaves, waveDisplayTitle, type Wave,
} from '../../../../core/domain/wave.ts';
import {
  MobileList, MobileListEmpty, MobileListItem, MobileListPage,
} from '../../ui/mobile-list/public.tsx';

export function MobileAreas({
  areas, wavesByArea, selectedAreaId, motion, onSelectArea, onBack, onOpenWave,
}: Readonly<{
  areas: readonly Area[];
  wavesByArea: ReadonlyMap<string, readonly Wave[]>;
  selectedAreaId: string | null;
  motion: 'none' | 'forward' | 'back';
  onSelectArea: (areaId: string) => void;
  onBack: () => void;
  onOpenWave: (waveId: string) => void;
}>) {
  const rows = visibleAreas(areas);
  const selected = selectedAreaId === null ? undefined : rows.find((area) => area.id === selectedAreaId);

  if (selected !== undefined) {
    const waves = visibleWaves(wavesByArea.get(selected.id) ?? []);
    return (
      <MobileListPage title={selected.name} backLabel="Areas" motion={motion} onBack={onBack}>
        <MobileList title="Waves">
          {waves.map((wave) => (
            <MobileListItem
              key={wave.id}
              title={waveDisplayTitle(wave.title)}
              meta={lifecycleLabel(wave.lifecycle)}
              onSelect={() => onOpenWave(wave.id)}
            />
          ))}
          {waves.length === 0 && <MobileListEmpty>No waves in this area yet.</MobileListEmpty>}
        </MobileList>
      </MobileListPage>
    );
  }

  return (
    <MobileListPage title="Areas" motion={motion}>
      <MobileList>
        {rows.map((area) => {
          const waves = visibleWaves(wavesByArea.get(area.id) ?? []);
          return (
            <MobileListItem
              key={area.id}
              title={area.name}
              meta={`${waves.length} ${waves.length === 1 ? 'wave' : 'waves'}`}
              onSelect={() => onSelectArea(area.id)}
            />
          );
        })}
      </MobileList>
    </MobileListPage>
  );
}
