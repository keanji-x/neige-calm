import { MobileTrackList } from './mobile-track-list.tsx';
// Both navigation levels share their header and actions. Choosing an Area
// scopes the Track list; only opening a Track changes the page underneath.
import { useLayoutEffect, useRef } from 'react';
import { useState } from '../../ui/state/public.ts';
import { useSpringPresence } from '../../ui/motion/presence.ts';
import { visibleAreas, type Area } from '../../../../core/domain/area.ts';
import { areaPinnedTracks, type Track } from '../../../../core/domain/track.ts';
import { ErrorBox } from '../../ui/error-box/public.tsx';
import { Icon } from '../../ui/icon/public.tsx';
import { MobileList, MobileListEmpty, MobileListGroup } from '../../ui/mobile-list/public.tsx';
import { MobileNavigationCreateAction, MobileNavigationHeader, type MobileNavigationActions } from './mobile-navigation-header.tsx';
import styles from './mobile-navigation.module.css';
import { type TrackActionsProps } from '../../features/track/row/actions.tsx';
import { useUiPreferences } from '../providers/ui-preferences.tsx';

type MobileTracksProps = Readonly<{
  view: 'areas' | 'tracks';
  tracksBackLabel?: string;
  areas: readonly Area[];
  tracksByArea: ReadonlyMap<string, readonly Track[]>;
  areaId: string | undefined;
  currentTrackId: string | undefined;
  onSelectArea: (areaId: string) => void;
  onOpenTrack: (trackId: string) => void;
  /** The reader's receipt for a track, keyed exactly as the rail's (`sidebar.tsx`): one receipt, both surfaces. */
  isUnread: (track: Track) => boolean;
  trackActions?: (track: Track) => Omit<TrackActionsProps, 'track' | 'className'>;
  readError: string | null;
  readLoading: boolean;
  onRetryRead: () => void;
}> & MobileNavigationActions;

/** Stable page shells preserve the Areas scroll position through direct page changes. */
export function MobileTracks(props: MobileTracksProps) {
  const tracksPageRef = useRef<HTMLDivElement | null>(null);
  const { view, areaId } = props;
  const [tracksPresented, setTracksPresented] = useState(view === 'tracks');
  if (view === 'tracks' && !tracksPresented) setTracksPresented(true);
  useSpringPresence(tracksPageRef, null, view === 'tracks', tracksPresented, true,
    () => { setTracksPresented(false); }, value => ({ transform: `translateX(${(1 - value) * 100}%)` }));
  useLayoutEffect(() => {
    if (view === 'tracks' && tracksPageRef.current !== null) tracksPageRef.current.scrollTop = 0;
  }, [areaId, view]);
  return <div className={styles.pages}>
    <div data-nc-workspace-page="areas" className={`${styles.pageFrame} ${view === 'tracks' ? styles.coveredPage : ''}`}
      inert={view !== 'areas'} aria-hidden={view !== 'areas' || undefined}>
      <NavigationPage {...props} view="areas" />
    </div>
    <div ref={tracksPageRef} data-nc-workspace-page="tracks" className={`${styles.pageFrame} ${view === 'tracks' ? '' : tracksPresented ? styles.coveredPage : styles.inactivePage}`}
      inert={view !== 'tracks'} aria-hidden={view !== 'tracks' || undefined}>
      <NavigationPage {...props} view="tracks" />
    </div>
    <MobileNavigationCreateAction creationScope={view === 'areas' ? 'area' : 'track'}
      area={visibleAreas(props.areas).find((area) => area.id === areaId)} onCreateArea={props.onCreateArea} onNewTrack={props.onNewTrack} />
  </div>;
}

function NavigationPage({
  view, tracksBackLabel = 'Areas', areas, tracksByArea, areaId, currentTrackId, onBack, onCreateArea, onSelectArea, onEditArea, onOpenTrack, onNewTrack, onOpenSettings,
  isUnread, trackActions, readError, readLoading, onRetryRead,
}: MobileTracksProps) {
  const shown = visibleAreas(areas);
  const selected = shown.find((candidate) => candidate.id === areaId);
  const preferences = useUiPreferences();
  const tracks = areaPinnedTracks(selected === undefined ? [] : tracksByArea.get(selected.id) ?? [],
    (track) => preferences.areaTrackPinned(track.areaId, track.id));
  return <div className={styles.page}>
    <MobileNavigationHeader creationScope={view === 'areas' ? 'area' : 'track'} title={view === 'areas' ? 'Areas' : selected?.name ?? 'Tracks'}
      backLabel={view === 'areas' ? 'workspace' : tracksBackLabel} area={view === 'tracks' ? selected : undefined}
      showSettings={false} actionsPlacement="floating"
      onBack={onBack} onNewTrack={onNewTrack} onCreateArea={onCreateArea} onEditArea={onEditArea} onOpenSettings={onOpenSettings} />
    <div className={styles.content}>
      {readError !== null && <ErrorBox message={readError} onRetry={onRetryRead} />}
      {readLoading && <p role="status">Loading workspace…</p>}
      <MobileListGroup appearance="plain" label={view === 'areas' ? 'Areas' : 'Tracks'}>
        {view === 'areas' ? <MobileList className={styles.navigationList}>
          {shown.map((area) => <li key={area.id} className={styles.trackRow}>
            <button type="button" className={styles.track} aria-label={area.name}
              aria-current={area.id === selected?.id ? 'location' : undefined}
              onClick={() => onSelectArea(area.id)}>
              <span className={styles.trackIcon}><Icon name="folder" /></span>
              <span className={styles.trackCopy}><span className={styles.trackTitle}>{area.name}</span>
                {tracksByArea.has(area.id) && <span className={styles.trackMeta}>{tracksByArea.get(area.id)!.length} 个 Tracks{area.id === selected?.id && <span> · {new Date(area.updatedAt).toDateString() === new Date().toDateString() ? '今天' : new Date(area.updatedAt).toLocaleDateString('zh-CN', { year: 'numeric', month: '2-digit', day: '2-digit' }).replaceAll('/', '-')}更新</span>}</span>}
              </span>
              <span className={styles.trackArrow}><Icon name="chevron-right" /></span>
            </button>
          </li>)}
          {shown.length === 0 && !readLoading && readError === null && <MobileListEmpty>No areas yet.</MobileListEmpty>}
        </MobileList> : <MobileTrackList tracks={tracks} currentTrackId={currentTrackId} onOpenTrack={onOpenTrack}
          isUnread={isUnread} trackActions={trackActions} emptyMessage={readError === null && !readLoading ? 'No tracks in this area yet.' : undefined} />}
      </MobileListGroup>
    </div>
  </div>;
}
