// Both navigation levels share their header and actions. Choosing an Area
// scopes the Track list; only opening a Track changes the page underneath.
import { useLayoutEffect, useRef } from 'react';
import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { List, ListItem } from '@astryxdesign/core/List';
import { activityNameBit } from '../../../../core/domain/activity.ts';
import { visibleAreas, type Area } from '../../../../core/domain/area.ts';
import { lifecycleLabel, trackActivityState, visibleTracks, trackDisplayTitle, type Track } from '../../../../core/domain/track.ts';
import { ActivityIndicator } from '../../ui/activity-indicator/public.tsx';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { MobileList, MobileListEmpty, MobileListGroup } from '../../ui/mobile-list/public.tsx';
import { MobileNavigationHeader, type MobileNavigationActions } from './mobile-navigation-header.tsx';
import styles from './mobile-navigation.module.css';

type MobileTracksProps = Readonly<{
  view: 'areas' | 'tracks';
  areas: readonly Area[];
  tracksByArea: ReadonlyMap<string, readonly Track[]>;
  areaId: string | undefined;
  currentTrackId: string | undefined;
  onSelectArea: (areaId: string) => void;
  onOpenTrack: (trackId: string) => void;
  /** The reader's receipt for a track, keyed exactly as the rail's (`sidebar.tsx`): one receipt, both surfaces. */
  isUnread: (track: Track) => boolean;
  readError: string | null;
  readLoading: boolean;
  onRetryRead: () => void;
}> & MobileNavigationActions;

/** Stable page shells preserve the Areas scroll position through direct page changes. */
export function MobileTracks(props: MobileTracksProps) {
  const tracksPageRef = useRef<HTMLDivElement | null>(null);
  const { view, areaId } = props;
  useLayoutEffect(() => {
    if (view === 'tracks' && tracksPageRef.current !== null) tracksPageRef.current.scrollTop = 0;
  }, [areaId, view]);
  return <div className={styles.pages}>
    <div data-nc-workspace-page="areas" className={`${styles.pageFrame} ${view === 'tracks' ? styles.inactivePage : ''}`}
      inert={view !== 'areas'} aria-hidden={view !== 'areas' || undefined}>
      <NavigationPage {...props} view="areas" />
    </div>
    <div ref={tracksPageRef} data-nc-workspace-page="tracks" className={`${styles.pageFrame} ${view === 'tracks' ? '' : styles.inactivePage}`}
      inert={view !== 'tracks'} aria-hidden={view !== 'tracks' || undefined}>
      <NavigationPage {...props} view="tracks" />
    </div>
  </div>;
}

function NavigationPage({
  view, areas, tracksByArea, areaId, currentTrackId, onBack, onCreateArea, onSelectArea, onEditArea, onOpenTrack, onNewTrack, onOpenSettings,
  isUnread, readError, readLoading, onRetryRead,
}: MobileTracksProps) {
  const shown = visibleAreas(areas);
  const selected = shown.find((candidate) => candidate.id === areaId);
  const tracks = selected === undefined ? [] : visibleTracks(tracksByArea.get(selected.id) ?? []);
  return <div className={styles.page}>
    <MobileNavigationHeader creationScope={view === 'areas' ? 'area' : 'track'} title={view === 'areas' ? 'Areas' : selected?.name ?? 'Tracks'}
      backLabel={view === 'areas' ? 'workspace' : 'Areas'} area={selected}
      onBack={onBack} onNewTrack={onNewTrack} onCreateArea={onCreateArea} onEditArea={onEditArea} onOpenSettings={onOpenSettings} />
    <div className={styles.content}>
      {readError !== null && <ErrorBox message={readError} onRetry={onRetryRead} />}
      {readLoading && <p role="status">Loading workspace…</p>}
      <MobileListGroup label={view === 'areas' ? 'Areas' : 'Tracks'}>
        {view === 'areas' ? <List density="balanced" className={styles.actionList}>
          {shown.map((area) => <ListItem key={area.id} label={area.name} className={styles.actionRow}
            isSelected={area.id === selected?.id} aria-current={area.id === selected?.id ? 'location' : undefined}
            startContent={<span className={styles.areaIcon}><Icon name="folder" /></span>}
            endContent={<AstryxIcon icon="chevronRight" size="sm" color="secondary" />}
            onClick={() => onSelectArea(area.id)} />)}
          {shown.length === 0 && !readLoading && readError === null && <MobileListEmpty>No areas yet.</MobileListEmpty>}
        </List> : <MobileList>
          {tracks.map((track) => {
            /* The same state the rail row shows (INV-APP-118): the kernel's
               activity overlay plus this reader's receipt, never the lifecycle.
               The name's activity bit comes from that SAME value as the dot,
               and the lifecycle phrase stays in `trackMeta` as the phase it is.
               The indicator is decorative here: the button's name carries the
               fact, so the primitive is not given anything to speak. */
            const activity = trackActivityState(track, isUnread(track));
            const activityBit = activityNameBit(activity);
            return <li key={track.id}>
              <button type="button" className={styles.track} aria-label={`${trackDisplayTitle(track.title)}${activityBit ? `, ${activityBit}` : ''}`}
                aria-current={track.id === currentTrackId ? 'page' : undefined} onClick={() => onOpenTrack(track.id)}>
                <span className={styles.trackIcon}><Icon name="file" /></span>
                <span className={styles.trackCopy}><span>{trackDisplayTitle(track.title)}</span><span className={styles.trackMeta}>{lifecycleLabel(track.lifecycle)}</span></span>
                {activity !== 'quiet' && <span className={styles.trackActivity} aria-hidden="true"><ActivityIndicator state={activity} /></span>}
              </button>
            </li>;
          })}
          {readError === null && !readLoading && tracks.length === 0 && <MobileListEmpty>No tracks in this area yet.</MobileListEmpty>}
        </MobileList>}
      </MobileListGroup>
    </div>
  </div>;
}
