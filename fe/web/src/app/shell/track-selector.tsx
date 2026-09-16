import { Button } from '@astryxdesign/core/Button';
import { Icon as AstryxIcon } from '@astryxdesign/core/Icon';
import { trackDisplayTitle, type Track } from '../../../../core/domain/track.ts';
import { Icon } from '../../ui/icon/public.tsx';
import { Menu } from '../../ui/menu/public.tsx';
import type { EditableTitleProps } from '../../ui/editable-title/public.tsx';
import styles from './area-selector.module.css';

/** Navigation is app-owned; editing remains owned by the existing title control. */
export function TrackSelector({ track, tracks, loading, error, onRetry, onSelectTrack, controls }: Readonly<{
  track: Track;
  tracks: readonly Track[];
  loading: boolean;
  error: string | null;
  onRetry: () => void;
  onSelectTrack: (trackId: string) => void;
  controls: Parameters<NonNullable<EditableTitleProps['readView']>>[0];
}>) {
  return <div className={styles.trackIdentity}>
    <Menu wrapClassName={styles.area} menuClassName={styles.menu} itemClassName={styles.item}
      items={[...tracks.map((choice) => ({ label: trackDisplayTitle(choice.title), current: choice.id === track.id, icon: <Icon name="file" />,
        onSelect: () => { if (choice.id !== track.id) onSelectTrack(choice.id); } })),
        ...(error === null ? [] : [{ label: `Could not read tracks: ${error}`, disabled: true, onSelect: () => undefined }, { label: 'Retry', onSelect: onRetry }]),
        ...(tracks.length > 0 || error !== null ? [] : [{ label: loading ? 'Loading tracks…' : 'No tracks in this area yet.', disabled: true, onSelect: () => undefined }]),
      ]}
      trigger={(props) => <h1 className={styles.trackHeading}><Button {...props} ref={(node) => { props.ref(node); controls.titleRef(node); }} label={trackDisplayTitle(track.title)}
        variant="ghost" size="lg" className={styles.button} data-nc-page-title=""
        aria-label={`Switch track, ${trackDisplayTitle(track.title)}`}
        onKeyDown={(event) => { if (event.key === 'F2') { event.preventDefault(); controls.beginEditing(); } }}
        endContent={<AstryxIcon icon="chevronDown" size="sm" color="inherit" />} /></h1>} />
  </div>;
}
