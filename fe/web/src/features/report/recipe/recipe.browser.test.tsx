// The recipe body editor, for real, in a real engine.
//
// Every other assertion about this screen lives in the jsdom tier with the
// widget substituted, because CodeMirror measures a layout jsdom does not
// have. That substitution is sound for what those cases assert — the save
// payload, the post-save render, the conflict — and it proves nothing about
// the one thing it replaces: that the editor actually mounts, actually shows
// the recipe, and that what the reader types actually reaches the draft the
// Save button sends.
//
// That gap is not hypothetical. An editor wired to the wrong prop, or one
// whose `onChange` never reaches state, passes every jsdom case in this slice
// untouched and is broken on screen. This file is the case that would not
// pass.
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
    /* Typed by its parameter rather than by a cast on `mock.calls`: the draft
       is the assertion's subject, and an `as` there would let the shape drift
       without the test noticing. `draft` is read below, so nothing is unused. */
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
    /* CodeMirror's editable is a `contenteditable`, so it resolves as a
       textbox by the accessible name the widget stamps on it — the same name
       the jsdom substitute carries, which is what keeps the two tiers talking
       about one control. */
    const field = await screen.findByRole('textbox', { name: 'Recipe body, Markdown' });
    expect(field.textContent).toContain('Ship checklist');

    /* Typed into the real engine: the click places a selection, and the
       keystrokes go through CodeMirror's own input handling — which is
       precisely the path a substitute cannot exercise. */
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
