// The recipe body editor, for real, in a real engine: CodeMirror measures a layout jsdom does not have.
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { TrackRecipe } from '../../../../../core/domain/track.ts';
import { RecipeEditor, type RecipeDraft } from './public.tsx';

afterEach(() => { document.body.replaceChildren(); });

const RECIPE: TrackRecipe = {
  id: 'r-ship', title: 'Ship checklist', body: '## Ship checklist\n',
  revision: 3, created_at: 1, updated_at: 2,
};

describe('the recipe body editor in a browser', () => {
  it('shows the stored body and sends what the reader typed into it', async () => {
    const user = userEvent.setup();
    const onWrite = vi.fn((draft: RecipeDraft) =>
      Promise.resolve({ kind: 'saved' as const, recipe: { ...RECIPE, body: draft.body } }));
    render(
      <RecipeEditor
        recipe={RECIPE}
        theme="light"
        onWrite={onWrite}
        onDelete={null}
        onClose={() => {}}
        onCreated={null}
      />,
    );

    await user.click(screen.getByRole('button', { name: 'Edit' }));
    const field = await screen.findByRole('textbox', { name: 'Recipe body, Markdown' });
    expect(field.textContent).toContain('Ship checklist');

    await user.click(field);
    await user.keyboard('{Control>}{End}{/Control}');
    await user.keyboard('One more line.');
    await user.click(screen.getByRole('button', { name: 'Save' }));

    expect(onWrite).toHaveBeenCalledTimes(1);
    const draft = onWrite.mock.calls[0][0];
    expect(draft.body).toContain('One more line.');
    expect(draft.body).toContain('Ship checklist');
    expect(draft.if_revision).toBe(3);
  });
});
