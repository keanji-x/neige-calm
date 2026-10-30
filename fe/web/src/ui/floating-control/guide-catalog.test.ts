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

it('accepts plain-text outcomes and complete examples but rejects executable or incomplete examples', () => {
  const step = { id: 'step', target: 'draft', title: 'Write', description: 'Try a request', advance: 'next',
    outcome: 'Review the draft before sending.', example: { label: 'Fill example', summary: 'A local page', text: 'Create a local page.' } };
  const input = { version: 1, tours: [{ id: 'tour', title: 'First task', priority: 1, exclusive: true, requires: [], steps: [step] }] };
  expect(parseGuideCatalog(input).tours[0].steps[0].example?.text).toBe('Create a local page.');
  for (const example of [{ label: 'Fill' }, { ...step.example, action: 'submit()' }]) {
    expect(() => parseGuideCatalog({ ...input, tours: [{ ...input.tours[0], steps: [{ ...step, example }] }] })).toThrow();
  }
});

it('accepts variant targets and plain-text location contracts', () => {
  const step = { id: 'step', target: ['wide', 'compact'], title: 'Hi', description: 'Read me', advance: 'next', locate: 'Open the sidebar', via: ['sidebar'] };
  const input = (value: unknown) => ({ version: 1, tours: [{ id: 'tour', title: 'Tour', priority: 1, exclusive: false, requires: [], steps: [value] }] });
  expect(parseGuideCatalog(input(step)).tours[0].steps[0]).toEqual(step);
  for (const value of [{ ...step, target: [] }, { ...step, via: ['#selector'] }, { ...step, locate: '' }]) {
    expect(() => parseGuideCatalog(input(value))).toThrow();
  }
});

it('deep-freezes the parsed catalog including variant and location arrays', () => {
  const catalog = parseGuideCatalog({ version: 1, tours: [{ id: 'tour', title: 'Tour', priority: 1, exclusive: false, requires: ['ready'], steps: [
    { id: 'step', target: ['wide', 'compact'], via: ['opener'], title: 'Hi', description: 'Read', advance: 'next', example: { label: 'Fill', summary: 'Example', text: 'Draft' } },
  ] }] });
  const tour = catalog.tours[0]; const step = tour.steps[0];
  for (const value of [catalog, catalog.tours, tour, tour.requires, tour.steps, step, step.target, step.via, step.example]) {
    expect(Object.isFrozen(value)).toBe(true);
  }
});
