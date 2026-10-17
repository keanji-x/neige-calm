import { expect, it } from 'vitest';
import { parseGuideCatalog } from './guide-catalog.ts';

it('accepts the shipped guide data and requires complete plain-text steps', () => {
  expect(parseGuideCatalog({ version: 1, tours: [{ id: 'one', title: 'Tour', priority: 1, exclusive: false, requires: [], steps: [{ id: 'one', target: 'button', title: 'Hi', description: 'Read me', advance: 'next' }] }] }).tours).toHaveLength(1);
  expect(() => parseGuideCatalog({ version: 1, tours: [{ id: 'x', title: 'Tour', priority: 1, steps: [{ id: 'x', target: 'button', title: 'Hi' }] }] })).toThrow();
});

it('rejects duplicate identities and executable configuration', () => {
  const step = { id: 'step', target: 'button', title: 'Hi', description: 'Read me', advance: 'next' };
  const tour = { id: 'tour', title: 'Tour', priority: 1, exclusive: false, requires: [], steps: [step] };
  expect(() => parseGuideCatalog({ version: 1, tours: [tour, tour] })).toThrow(/Duplicate/);
  expect(() => parseGuideCatalog({ version: 1, tours: [{ ...tour, steps: [step, step] }] })).toThrow(/Duplicate/);
  expect(() => parseGuideCatalog({ version: 1, tours: [{ ...tour, steps: [{ ...step, script: 'run()' }] }] })).toThrow();
});
