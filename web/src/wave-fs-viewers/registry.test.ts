import { afterEach, describe, expect, it } from 'vitest';
import {
  __resetWaveFsViewerRegistryForTest,
  registerWaveFsViewer,
  resolveWaveFsViewer,
  type WaveFsViewer,
} from './registry';

function makeViewer(
  id: string,
  match: (path: string) => boolean,
): WaveFsViewer<string> {
  return {
    id,
    match,
    parse: (raw) => raw,
    Component: () => null,
  };
}

afterEach(() => {
  __resetWaveFsViewerRegistryForTest();
});

describe('wave fs viewer registry', () => {
  it('registers and resolves a matching viewer', () => {
    registerWaveFsViewer(makeViewer('wave-json', (path) => path === 'wave.json'));

    expect(resolveWaveFsViewer('wave.json')?.id).toBe('wave-json');
  });

  it('returns null when no viewer matches', () => {
    registerWaveFsViewer(makeViewer('wave-json', (path) => path === 'wave.json'));

    expect(resolveWaveFsViewer('cards/index.json')).toBeNull();
  });

  it('uses the first registered viewer when matches overlap', () => {
    registerWaveFsViewer(makeViewer('first', (path) => path.endsWith('.json')));
    registerWaveFsViewer(makeViewer('second', (path) => path.endsWith('.json')));

    expect(resolveWaveFsViewer('cards/index.json')?.id).toBe('first');
  });
});
