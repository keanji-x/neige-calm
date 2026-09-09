import { expect, it } from 'vitest';
import type { ApiAbortSignal } from '../api/types.js';
import { recipePreviewOperation, recipePreviewSchema } from './recipe-preview.js';

it('decodes a compiled payload directly without a Card and requires preview identity', () => {
  const wire = { id: 'saved', revision: 2, payload: { summary: 'Saved', body: '# Body', blocks: [
    { id: 'p', kind: 'prose', payload: { markdown: '# Body' } },
  ] } };
  expect(recipePreviewSchema.parse(wire)).toEqual({ id: 'saved', revision: 2,
    report: { summary: 'Saved', body: '# Body', blocks: [{ id: 'p', kind: 'prose', payload: { markdown: '# Body' } }] },
  });
  for (const invalid of [{ ...wire, revision: undefined }, { ...wire, id: undefined },
    { ...wire, payload: { summary: 'Saved', body: '# Body' } }, { ...wire, payload: { blocks: [] } },
  ]) expect(recipePreviewSchema.safeParse(invalid).success).toBe(false);
});

it('pins the saved revision on a GET request and forwards cancellation', () => {
  const signal: ApiAbortSignal = { aborted: false, addEventListener() {}, removeEventListener() {} };
  const operation = recipePreviewOperation('a/b', 7, signal);
  expect(operation.method).toBe('GET');
  expect(operation.path).toBe('/api/track-recipes/a%2Fb/preview?if_revision=7');
  expect(operation.body).toBeUndefined();
  expect(operation.signal).toBe(signal);
});
