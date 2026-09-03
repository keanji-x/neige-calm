import { afterEach, describe, expect, it } from 'vitest';
import {
  __resetTrackFsViewerRegistryForTest,
  registerTrackFsViewer,
  resolveTrackFsViewer,
  type TrackFsViewer,
} from './registry';

function makeViewer(
  id: string,
  match: (path: string) => boolean,
): TrackFsViewer<string> {
  return {
    id,
    match,
    parse: (raw) => raw,
    Component: () => null,
  };
}

afterEach(() => {
  __resetTrackFsViewerRegistryForTest();
});

describe('track fs viewer registry', () => {
  it('registers and resolves a matching viewer', () => {
    registerTrackFsViewer(makeViewer('track-json', (path) => path === 'track.json'));

    expect(resolveTrackFsViewer('track.json')?.id).toBe('track-json');
  });

  it('returns null when no viewer matches', () => {
    registerTrackFsViewer(makeViewer('track-json', (path) => path === 'track.json'));

    expect(resolveTrackFsViewer('cards/index.json')).toBeNull();
  });

  it('uses the first registered viewer when matches overlap', () => {
    registerTrackFsViewer(makeViewer('first', (path) => path.endsWith('.json')));
    registerTrackFsViewer(makeViewer('second', (path) => path.endsWith('.json')));

    expect(resolveTrackFsViewer('cards/index.json')?.id).toBe('first');
  });
});
