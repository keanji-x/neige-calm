import '../../styles/entry.css';
import { it } from 'vitest';
import { checkCoordinates } from './tests/coordinates-fixture.tsx';

it('commits three-level visual inputs and revisits phases without drift', () => checkCoordinates({ layers: 3, shift: 0 }), 45_000);
it('commits five-level fractional visual inputs against independent CSS rulers', () => checkCoordinates({ layers: 5, shift: -1 / 2048 }), 45_000);

// Keep cross-round compensation history at the finest authored CSS input.
it('commits five-level fine CSS visual inputs against independent CSS rulers', () => checkCoordinates({ layers: 5, shift: -3 / 131072, rounds: 3 }), 45_000);

for (const gap of [33.984375, 33.999, 34.007, 34.015625, 34.03125]) {
  it(`commits visual edges across gap boundary ${gap}`, () => checkCoordinates({ layers: 3, shift: 0, gap }), 45_000);
}
