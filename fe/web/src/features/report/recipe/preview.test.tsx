import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, expect, it } from 'vitest';

import { RecipePreview } from './preview.tsx';

afterEach(cleanup);

it.each([
  { id: 'another', revision: 7, summary: 'Saved title' },
  { id: 'saved', revision: 8, summary: 'Saved title' },
  { id: 'saved', revision: 7, summary: 'Another title' },
])('refuses mismatched saved preview metadata: %j', async ({ id, revision, summary }) => {
  const recipe = { id: 'saved', revision: 7, title: 'Saved title', body: 'Saved body', created_at: 1, updated_at: 1 };
  render(<RecipePreview recipe={recipe} load={() => Promise.resolve({ kind: 'ready', preview: {
    id, revision, report: { summary, body: 'Unmatched body', blocks: [{ id: 'p', kind: 'prose', payload: { markdown: 'Unmatched body' } }] },
  } })}/>);
  expect(await screen.findByRole('button', { name: '重试预览' })).toBeTruthy();
  expect(screen.queryByText('Unmatched body')).toBeNull();
});
