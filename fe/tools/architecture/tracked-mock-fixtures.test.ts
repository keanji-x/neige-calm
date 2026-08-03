import { mkdirSync, mkdtempSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { compareMockFixtureFiles, listMockFixtureFiles } from './tracked-mock-fixtures.mjs';

describe('tracked mock fixtures', () => {
  it('reports disk and Git differences in either direction', () => {
    expect(compareMockFixtureFiles(['tools/mock/fixtures/positive/a.json'], [])).toContain('differ from Git');
    expect(compareMockFixtureFiles([], ['tools/mock/fixtures/negative/b.json'])).toContain('differ from Git');
    expect(compareMockFixtureFiles(['b', 'a'], ['a', 'b'])).toBe('');
  });

  it('diagnoses a missing fixture directory', () => {
    const root = mkdtempSync(resolve(tmpdir(), 'neige-calm-fixtures-'));
    mkdirSync(resolve(root, 'positive'));
    expect(listMockFixtureFiles(root)).toEqual({ files: [], problem: 'mock fixture directory is missing: tools/mock/fixtures/negative' });
  });
});
