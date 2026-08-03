import { describe, expect, it } from 'vitest';
import { directoryInputValue, joinDirectoryPath, normalizeDirectoryPath } from './public.tsx';

describe('directory browser public contract', () => {
  it('freezes path seed and rough join semantics', () => {
    expect(normalizeDirectoryPath('/work///')).toBe('/work');
    expect(directoryInputValue('/work')).toBe('/work/');
    expect(directoryInputValue('/')).toBe('/');
    expect(joinDirectoryPath('/', 'src')).toBe('/src');
    expect(joinDirectoryPath('/work/', 'src')).toBe('/work/src');
  });
});
