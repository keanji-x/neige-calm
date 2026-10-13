// @vitest-environment node
import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

import { isHtmlPath } from './file-kind.ts';

/** The html block's shared kernel fixture: what the block accepts, the viewer frames, and the route serves as a page. */
const fixture = JSON.parse(readFileSync(new URL('../../../../../test-data/report-html-path-v1.json', import.meta.url), 'utf8')) as {
  accept: string[];
  refuse: string[];
};

describe('isHtmlPath', () => {
  it('frames every path the html block accepts', () => {
    expect(fixture.accept.filter((path) => !isHtmlPath(path))).toEqual([]);
  });

  // The route trims a path and reads a bare `.html` as a dotfile: neither is served as a page.
  it.each(['.html', 'docs/.html', 'docs/.HTM', ' .html', 'docs/a.html ', '\tdocs/a.html', 'docs/a.txt'])(
    'shows %j as source', (path) => {
      expect(isHtmlPath(path)).toBe(false);
    },
  );
});
