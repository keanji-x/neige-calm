import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { userEvent } from 'vitest/browser';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import type { TrackRecipe } from '../../../../../core/domain/track.ts';
import { RecipeEditor, RecipesPage, type RecipeDraft, type RecipeWriteOutcome } from './public.tsx';
import { loadRecipeBodyEditor } from './load-body-editor.ts';
import '../../../styles/entry.css';

vi.mock(import('./load-body-editor.ts'), async original => {
  const actual = await original();
  return { loadRecipeBodyEditor: vi.fn(actual.loadRecipeBodyEditor) };
});
const RECIPE: TrackRecipe = { id: 'recipe-a', title: 'Stored recipe', body: 'Original **Markdown**.', revision: 4, created_at: 1, updated_at: 2 };
const LABEL = 'Recipe body, Markdown';
const actualLoader = () => import('./body-editor.tsx');
beforeEach(() => { vi.mocked(loadRecipeBodyEditor).mockReset().mockImplementation(actualLoader); });
afterEach(cleanup);
function holdEditor() {
  let release!: () => void;
  const gate = new Promise<void>(done => { release = done; });
  vi.mocked(loadRecipeBodyEditor).mockImplementation(() => gate.then(actualLoader));
  return async () => { await act(async () => { release(); await gate; await actualLoader(); }); };
}
function editor(onWrite = vi.fn<(draft: RecipeDraft) => Promise<RecipeWriteOutcome>>()) {
  return render(<RecipeEditor recipe={RECIPE} theme="light" onWrite={onWrite} onDelete={null} onClose={() => {}} onCreated={null} />);
}

it('loads only on Edit and preserves title input and focus when the real editor arrives', async () => {
  const release = holdEditor();
  editor();
  expect(loadRecipeBodyEditor).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole('button', { name: 'Edit' }));
  try {
    expect(loadRecipeBodyEditor).toHaveBeenCalledTimes(1);
    expect(screen.getByText('Loading Markdown editor…')).toBeDefined();
    const title = screen.getByRole('textbox', { name: 'Recipe title' });
    await userEvent.fill(title, 'My unsaved title');
    expect(document.activeElement).toBe(title);
    await release();
    const body = await screen.findByRole('textbox', { name: LABEL });
    expect(body.textContent).toBe(RECIPE.body);
    expect((title as HTMLInputElement).value).toBe('My unsaved title');
    expect(document.activeElement).toBe(title);
  } finally { cleanup(); await release(); }
});

it('keeps explicit plain editing, selection and composition through late loading and a save conflict', async () => {
  const release = holdEditor();
  const onWrite = vi.fn<(draft: RecipeDraft) => Promise<RecipeWriteOutcome>>().mockResolvedValue({ kind: 'conflict' });
  editor(onWrite);
  await userEvent.click(screen.getByRole('button', { name: 'Edit' }));
  try {
    await userEvent.click(screen.getByRole('button', { name: 'Use plain text' }));
    const body = screen.getByRole<HTMLTextAreaElement>('textbox', { name: LABEL });
    expect(body.tagName).toBe('TEXTAREA');
    await userEvent.fill(body, 'Unsaved Markdown 日本語');
    body.setSelectionRange(3, 12, 'backward');
    fireEvent.compositionStart(body);
    await release();
    expect(screen.getByRole('textbox', { name: LABEL })).toBe(body);
    expect(document.activeElement).toBe(body);
    expect(body.value).toBe('Unsaved Markdown 日本語');
    expect([body.selectionStart, body.selectionEnd, body.selectionDirection]).toEqual([3, 12, 'backward']);
    fireEvent.compositionEnd(body, { data: '日本語' });
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    await screen.findByText(/This recipe changed somewhere else/);
    expect(onWrite).toHaveBeenCalledWith({ title: RECIPE.title, body: body.value, if_revision: 4 });
    expect(screen.getByRole('textbox', { name: LABEL })).toBe(body);
    expect(body.value).toBe('Unsaved Markdown 日本語');
    expect(loadRecipeBodyEditor).toHaveBeenCalledTimes(1);
  } finally { cleanup(); await release(); }
});

it('explains a failed module and saves plain text under the existing unconfirmed-create lock', async () => {
  vi.mocked(loadRecipeBodyEditor).mockRejectedValue(new Error('Chunk unavailable'));
  const onWrite = vi.fn<(draft: RecipeDraft) => Promise<RecipeWriteOutcome>>()
    .mockResolvedValueOnce({ kind: 'unconfirmed', message: 'Creation is unconfirmed.' })
    .mockResolvedValueOnce({ kind: 'saved', recipe: { ...RECIPE, body: 'Held plain draft' } });
  render(<RecipeEditor recipe={null} theme="light" onWrite={onWrite} onDelete={null} onClose={() => {}} onCreated={() => {}} />);
  await screen.findByText('The Markdown editor could not load. You can continue in plain text.');
  await userEvent.click(screen.getByRole('button', { name: 'Use plain text' }));
  const body = screen.getByRole<HTMLTextAreaElement>('textbox', { name: LABEL });
  await userEvent.fill(body, 'Held plain draft');
  await userEvent.click(screen.getByRole('button', { name: 'Save' }));
  await screen.findByText('Creation is unconfirmed.');
  expect(body.readOnly).toBe(true);
  await userEvent.type(body, ' forbidden change');
  expect(body.value).toBe('Held plain draft');
  expect(screen.getByRole<HTMLInputElement>('textbox', { name: 'Recipe title' }).disabled).toBe(true);
  await userEvent.click(screen.getByRole('button', { name: 'Try again' }));
  expect(onWrite).toHaveBeenCalledTimes(2);
  expect(onWrite.mock.calls[1][0]).toEqual(onWrite.mock.calls[0][0]);
  expect(loadRecipeBodyEditor).toHaveBeenCalledTimes(1);
});

it('drops an abandoned load and starts a fresh editor session after cancel', async () => {
  const release = holdEditor();
  render(<RecipesPage recipes={[RECIPE]} loaded refreshing={false} error={null} theme="light" onRetry={() => {}}
    onWrite={vi.fn()} onDelete={vi.fn()} />);
  await userEvent.click(screen.getByRole('button', { name: RECIPE.title }));
  await userEvent.click(screen.getByRole('button', { name: 'Edit' }));
  try {
    expect(screen.getByText('Loading Markdown editor…')).toBeDefined();
    await userEvent.click(screen.getByRole('button', { name: 'Use plain text' }));
    await userEvent.fill(screen.getByRole('textbox', { name: LABEL }), 'Discard this draft');
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    const edit = screen.getByRole('button', { name: 'Edit' });
    edit.focus();
    await release();
    expect(document.activeElement).toBe(edit);
    expect(screen.queryByRole('textbox', { name: LABEL })).toBeNull();
    await userEvent.click(edit);
    const body = await screen.findByRole('textbox', { name: LABEL });
    expect(body.tagName).not.toBe('TEXTAREA');
    expect(body.textContent).toBe(RECIPE.body);
    await waitFor(() => expect(loadRecipeBodyEditor).toHaveBeenCalledTimes(2));
  } finally { cleanup(); await release(); }
});


it('ignores an unresolved editor after leaving its recipe for a different draft', async () => {
  const release = holdEditor();
  render(<RecipesPage recipes={[RECIPE]} loaded refreshing={false} error={null} theme="light" onRetry={() => {}}
    onWrite={vi.fn()} onDelete={vi.fn()} />);
  await userEvent.click(screen.getByRole('button', { name: RECIPE.title }));
  await userEvent.click(screen.getByRole('button', { name: 'Edit' }));
  try {
    expect(screen.getByText('Loading Markdown editor…')).toBeDefined();
    await userEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    await userEvent.click(screen.getByRole('button', { name: 'All recipes' }));
    vi.mocked(loadRecipeBodyEditor).mockImplementation(actualLoader);
    await userEvent.click(screen.getByRole('button', { name: 'New recipe' }));
    await screen.findByRole('textbox', { name: LABEL });
    const title = screen.getByRole<HTMLInputElement>('textbox', { name: 'Recipe title' });
    await userEvent.fill(title, 'A different unsaved recipe');
    await release();
    expect(document.activeElement).toBe(title);
    expect(title.value).toBe('A different unsaved recipe');
    expect(screen.getByRole('textbox', { name: LABEL }).textContent).toBe('');
  } finally { cleanup(); await release(); }
});
