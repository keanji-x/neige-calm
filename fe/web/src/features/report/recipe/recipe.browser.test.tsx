// The recipe body editor, for real, in a real engine: CodeMirror measures a layout jsdom does not have.
import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { TrackRecipe } from '../../../../../core/domain/track.ts';
import { RecipeEditor, type RecipeDraft, type RecipeWriteOutcome } from './public.tsx';

afterEach(() => { cleanup(); document.body.replaceChildren(); });

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

  /* #2175-18: while a create is unconfirmed the held draft is on screen, so the real CodeMirror takes no typing; the
     jsdom tier swaps the editor for a textarea and cannot show this. */
  it('takes no typing while a create is unconfirmed, and Try again resends the held draft', async () => {
    const user = userEvent.setup();
    const saved = { ...RECIPE, body: 'Held draft.' };
    const onWrite = vi.fn<(draft: RecipeDraft) => Promise<RecipeWriteOutcome>>()
      .mockResolvedValueOnce({ kind: 'unconfirmed', message: 'Creating the recipe is unconfirmed.' })
      .mockResolvedValueOnce({ kind: 'saved', recipe: saved });
    render(<RecipeEditor recipe={null} theme="light" onWrite={onWrite} onDelete={null} onClose={() => {}} onCreated={() => {}} />);

    const field = await screen.findByRole('textbox', { name: 'Recipe body, Markdown' });
    await user.click(field);
    await user.keyboard('Held draft.');
    await user.click(screen.getByRole('button', { name: 'Save' }));
    expect(await screen.findByText('Creating the recipe is unconfirmed.')).toBeTruthy();

    expect(field.getAttribute('aria-readonly')).toBe('true');
    await user.click(field);
    await user.keyboard('{Control>}{End}{/Control} typed while held');
    expect(field.textContent).toBe('Held draft.');

    await user.click(screen.getByRole('button', { name: 'Try again' }));
    expect(onWrite).toHaveBeenCalledTimes(2);
    expect(onWrite.mock.calls[1][0].body).toBe('Held draft.');
  });
});
