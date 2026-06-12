import { registerWaveFsViewer } from './registry';
import { CardsIndexViewer } from './builtins/cards-index-viewer';
import { CardMetaViewer } from './builtins/card-meta-viewer';
import { CardRuntimeViewer } from './builtins/card-runtime-viewer';
import { HookEventsViewer } from './builtins/hook-events-viewer';
import { RunDetailViewer } from './builtins/run-detail-viewer';
import { RunsIndexViewer } from './builtins/runs-index-viewer';
import { WaveInfoViewer } from './builtins/wave-info-viewer';

export { useWaveFsViewer } from './useWaveFsViewer';
export type { WaveFsViewer } from './registry';

registerWaveFsViewer(CardsIndexViewer);
registerWaveFsViewer(WaveInfoViewer);
registerWaveFsViewer(CardMetaViewer);
registerWaveFsViewer(HookEventsViewer);
registerWaveFsViewer(CardRuntimeViewer);
registerWaveFsViewer(RunsIndexViewer);
registerWaveFsViewer(RunDetailViewer);
