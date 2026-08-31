import { useEffect } from 'react';

import type { Cove } from '../../../../core/domain/cove.ts';
import { visibleCoves } from '../../../../core/domain/cove.ts';
import {
  lifecycleLabel, visibleWaves, waveDisplayTitle, type Wave,
} from '../../../../core/domain/wave.ts';
import {
  MobileList, MobileListEmpty, MobileListItem, MobileListPage,
} from '../../ui/mobile-list/public.tsx';
import { setMobileSecondaryOpen } from '../../ui/mobile-page/public.ts';
import { useState } from '../../ui/state/public.ts';

export function MobileCoves({ coves, wavesByCove, initialCoveId = null, onOpenWave }: Readonly<{
  coves: readonly Cove[];
  wavesByCove: ReadonlyMap<string, readonly Wave[]>;
  initialCoveId?: string | null;
  onOpenWave: (waveId: string) => void;
}>) {
  const [selectedCoveId, setSelectedCoveId] = useState<string | null>(initialCoveId);
  const [motion, setMotion] = useState<'none' | 'forward' | 'back'>('none');
  const rows = visibleCoves(coves);
  const selected = selectedCoveId === null ? undefined : rows.find((cove) => cove.id === selectedCoveId);

  useEffect(() => setMobileSecondaryOpen(selectedCoveId !== null), [selectedCoveId]);
  useEffect(() => () => setMobileSecondaryOpen(false), []);

  if (selected !== undefined) {
    const waves = visibleWaves(wavesByCove.get(selected.id) ?? []);
    return (
      <MobileListPage title={selected.name} backLabel="Coves" motion={motion} onBack={() => {
        setMotion('back');
        setSelectedCoveId(null);
      }}>
        <MobileList title="Waves">
          {waves.map((wave) => (
            <MobileListItem
              key={wave.id}
              title={waveDisplayTitle(wave.title)}
              description={lifecycleLabel(wave.lifecycle)}
              meta={wave.pinnedAt === null ? undefined : 'Pinned'}
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
              description={`${waves.length} ${waves.length === 1 ? 'wave' : 'waves'}`}
              onSelect={() => {
                setMotion('forward');
                setSelectedCoveId(cove.id);
              }}
            />
          );
        })}
      </MobileList>
    </MobileListPage>
  );
}
