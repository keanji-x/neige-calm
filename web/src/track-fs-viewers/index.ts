import { registerTrackFsViewer } from './registry';
import { CardsIndexViewer } from './builtins/cards-index-viewer';
import { CardMetaViewer } from './builtins/card-meta-viewer';
import { CardRuntimeViewer } from './builtins/card-runtime-viewer';
import { HookEventsViewer } from './builtins/hook-events-viewer';
import { RunDetailViewer } from './builtins/run-detail-viewer';
import { RunsIndexViewer } from './builtins/runs-index-viewer';
import { TrackInfoViewer } from './builtins/track-info-viewer';

export { useTrackFsViewer } from './useTrackFsViewer';
export type { TrackFsViewer } from './registry';

registerTrackFsViewer(CardsIndexViewer);
registerTrackFsViewer(TrackInfoViewer);
registerTrackFsViewer(CardMetaViewer);
registerTrackFsViewer(HookEventsViewer);
registerTrackFsViewer(CardRuntimeViewer);
registerTrackFsViewer(RunsIndexViewer);
registerTrackFsViewer(RunDetailViewer);
