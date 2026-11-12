import '../../styles/entry.css';
import { it } from 'vitest';
import { checkCoordinates } from './tests/coordinates-fixture.tsx';

for (const width of [390, 1440]) {
  // scale=1 and zoom=1 resolve to the same complete helper parameters.
  it(`commits visual edges at ${width} identity`, () => checkCoordinates({ layers: 5,
    shift: 1 / 2048, width, inline: true }), 45_000);
  for (const mode of ['scale', 'zoom'] as const) for (const factor of [0.75, 1.125, 1.5]) {
    it(`commits visual edges at ${width} ${mode} ${factor}`, () => checkCoordinates({ layers: 5,
      shift: 1 / 2048, width, [mode]: factor, inline: true }), 45_000);
  }
}
