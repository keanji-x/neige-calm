import { it } from 'vitest';
import { assertSourceRemainsGuarded } from './fixture-oracle.mjs';

it.skipIf(!process.env.MUTATION_FIXTURE_SOURCE)('source remains guarded', () => {
  assertSourceRemainsGuarded(process.env.MUTATION_FIXTURE_SOURCE!);
});
