import { coveOf, visibleCoves, type Cove } from '../../../../core/domain/cove.ts';
import {
  lifecycleLabel, visibleWaves, waveDisplayTitle, type Wave,
} from '../../../../core/domain/wave.ts';
import {
  MobileList, MobileListEmpty, MobileListItem, MobileListPage,
} from '../../ui/mobile-list/public.tsx';

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
  const descriptionOf = (wave: Wave) => {
    const cove = coveOf(wave.coveId, shownCoves);
    return `${cove?.name ?? 'Unknown cove'} · ${lifecycleLabel(wave.lifecycle)}`;
  };

  return (
    <MobileListPage title="Pages">
      {pinned.length > 0 && (
        <MobileList title="Pinned">
          {pinned.map((wave) => (
            <MobileListItem
              key={wave.id}
              title={waveDisplayTitle(wave.title)}
              description={descriptionOf(wave)}
              onSelect={() => onOpenWave(wave.id)}
            />
          ))}
        </MobileList>
      )}
      <MobileList title="Recently updated">
        {recent.map((wave) => (
          <MobileListItem
            key={wave.id}
            title={waveDisplayTitle(wave.title)}
            description={descriptionOf(wave)}
            onSelect={() => onOpenWave(wave.id)}
          />
        ))}
        {recent.length === 0 && pinned.length === 0 && <MobileListEmpty>No Pages yet.</MobileListEmpty>}
      </MobileList>
    </MobileListPage>
  );
}
