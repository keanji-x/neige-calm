import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';

const source = readFileSync(resolve(import.meta.dirname, 'pending-route.tsx'), 'utf8');

describe('missing-route focus destination', () => {
  it('keeps the replacement h1 programmatically focusable and discoverable', () => {
    expect(source).toMatch(/<h1[^>]*data-nc-page-title=""[^>]*tabIndex=\{-1\}/);
  });
});
