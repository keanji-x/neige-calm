import { readFileSync } from 'node:fs';
import { expect, it } from 'vitest';

it.skipIf(!process.env.MUTATION_FIXTURE_SOURCE)('source remains guarded', () => {
  expect(readFileSync(process.env.MUTATION_FIXTURE_SOURCE!, 'utf8')).toContain('export const guarded = true;');
});
