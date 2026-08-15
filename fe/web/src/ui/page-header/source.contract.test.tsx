import { readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

function productionTypeScriptUnder(directory: string): string[] {
  return readdirSync(directory, { withFileTypes: true }).flatMap((entry) => {
    const path = resolve(directory, entry.name);
    if (entry.isDirectory()) return productionTypeScriptUnder(path);
    return entry.isFile() && /\.tsx?$/.test(entry.name)
      && !/\.(?:test|spec)\.tsx?$/.test(entry.name) ? [path] : [];
  });
}

describe('page-header scroll seam source contract', () => {
  it('has no production data-nc-scrolled writer until the §6.4 listener lands', () => {
    const sourceRoot = resolve(import.meta.dirname, '../..');
    // Deliberately broad and fail-closed: TypeScript has no reader for this
    // CSS-consumed seam, so either spelling anywhere means a writer. Prefer a
    // false positive over missing an unfamiliar write form. Delete this test
    // when the shared §6.4 listener is implemented.
    const spellings = /data-nc-scrolled|ncScrolled/;
    const files = productionTypeScriptUnder(sourceRoot);
    // Canary against directory discovery being silently narrowed while still
    // finding enough local production files to leave the scan green.
    expect(files.length).toBeGreaterThan(20);
    const matches = files
      .filter((path) => spellings.test(readFileSync(path, 'utf8')));
    expect(matches).toEqual([]);
  });
});
