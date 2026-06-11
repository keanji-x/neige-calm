import { registerWaveFsViewer } from './registry';
import { CardsIndexViewer } from './builtins/cards-index-viewer';

export { useWaveFsViewer } from './useWaveFsViewer';
export type { WaveFsViewer } from './registry';

registerWaveFsViewer(CardsIndexViewer);
