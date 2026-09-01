// The Coves sheet: the cove list, and one cove's waves.
//
// Presentational. The drill-in used to live here as `selectedCoveId` plus a
// coupled `motion`, and the shell learned about it through a `window`
// CustomEvent — three owners for one transition. Both now sit in `app/shell`
// as a single state (#1191 §2.2), because the shell is what has to derive
// "a secondary page is showing" from it and what has to restore it when the
// reader returns from a report with `?from=cove`.
//
// The unmount cleanup that used to publish "secondary closed" is gone with the
// event: the shell's formula conjoins `mobileSection === 'coves'`, so a sheet
// that is not rendered cannot claim the screen.

import type { Cove } from '../../../../core/domain/cove.ts';
import { visibleCoves } from '../../../../core/domain/cove.ts';
import {
  lifecycleLabel, visibleWaves, waveDisplayTitle, type Wave,
} from '../../../../core/domain/wave.ts';
import {
  MobileList, MobileListEmpty, MobileListItem, MobileListPage,
} from '../../ui/mobile-list/public.tsx';

export function MobileCoves({
  coves, wavesByCove, selectedCoveId, motion, onSelectCove, onBack, onOpenWave,
}: Readonly<{
  coves: readonly Cove[];
  wavesByCove: ReadonlyMap<string, readonly Wave[]>;
  selectedCoveId: string | null;
  motion: 'none' | 'forward' | 'back';
  onSelectCove: (coveId: string) => void;
  onBack: () => void;
  onOpenWave: (waveId: string) => void;
}>) {
  const rows = visibleCoves(coves);
  const selected = selectedCoveId === null ? undefined : rows.find((cove) => cove.id === selectedCoveId);

  if (selected !== undefined) {
    const waves = visibleWaves(wavesByCove.get(selected.id) ?? []);
    return (
      <MobileListPage title={selected.name} backLabel="Coves" motion={motion} onBack={onBack}>
        <MobileList title="Waves">
          {waves.map((wave) => (
            <MobileListItem
              key={wave.id}
              title={waveDisplayTitle(wave.title)}
              meta={lifecycleLabel(wave.lifecycle)}
              onSelect={() => onOpenWave(wave.id)}
            />
          ))}
          {waves.length === 0 && <MobileListEmpty>No waves in this cove yet.</MobileListEmpty>}
        </MobileList>
      </MobileListPage>
    );
  }

  return (
    <MobileListPage title="Coves" motion={motion}>
      <MobileList>
        {rows.map((cove) => {
          const waves = visibleWaves(wavesByCove.get(cove.id) ?? []);
          return (
            <MobileListItem
              key={cove.id}
              title={cove.name}
              meta={`${waves.length} ${waves.length === 1 ? 'wave' : 'waves'}`}
              onSelect={() => onSelectCove(cove.id)}
            />
          );
        })}
      </MobileList>
    </MobileListPage>
  );
}
