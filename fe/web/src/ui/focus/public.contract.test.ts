import { describe, expect, it } from 'vitest';
import { findTypeaheadMatch, normalizeTypeaheadLabel } from './public.ts';

describe('focus public contract', () => {
  it('normalizes labels and cycles prefix matches', () => {
    expect(normalizeTypeaheadLabel('  ALPHA ')).toBe('alpha');
    expect(findTypeaheadMatch(['Alpha', 'Beta', 'Alpine'], 'al', 0)).toBe(0);
    expect(findTypeaheadMatch(['Alpha', 'Beta', 'Alpine'], 'a', 0)).toBe(2);
    expect(findTypeaheadMatch(['Alpha', 'Beta', 'Alpine'], 'b', 2)).toBe(1);
  });
});
