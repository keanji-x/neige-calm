// Recipes: the user's own saved starting points for a new track.
// After a save the view renders the server's response, never the local draft; a 409 keeps the draft.

import { Banner } from '@astryxdesign/core/Banner';
import { Button } from '@astryxdesign/core/Button';
import { TextInput } from '@astryxdesign/core/TextInput';
import { useId } from 'react';

import type { TrackRecipe } from '../../../../../core/domain/track.ts';
import { ConfirmDialog } from '../../../ui/dialog/public.tsx';
import { useDeleteConfirm } from '../../../ui/operation-feedback/public.tsx';
import { useState } from '../../../ui/state/public.ts';
import { RecipeBodyEditor, type RecipeEditorTheme } from './body-editor.tsx';
import { ProseBlock } from '../document/public.tsx';
import styles from './recipe.module.css';

export type { RecipeEditorTheme };

/** What a write attempt came back as. `conflict` is its own state because the editor stays in edit mode holding the draft. */
export type RecipeWriteOutcome =
  | Readonly<{ kind: 'saved'; recipe: TrackRecipe }>
  | Readonly<{ kind: 'conflict' }>
  | Readonly<{ kind: 'failed'; message: string }>;

/** `if_revision: null` is a create; a number is a `PUT` on that revision. */
export type RecipeDraft = Readonly<{ title: string; body: string; if_revision: number | null }>;

const CONFLICT_NOTICE
  = 'This recipe changed somewhere else since you opened it. Your edits are still here — '
  + 'copy anything you need, then close and reopen the recipe to start from the current version.';

const NEW_RECIPE_TITLE = 'Untitled recipe';

/** The body editable's accessible name: used by both the visible label and the `aria-label`, which must not drift. */
const BODY_FIELD_LABEL = 'Recipe body, Markdown';

const DELETE_TITLE = 'Delete this recipe?';
const DELETE_DESCRIPTION
  = 'The recipe is removed. Tracks already created from it keep their reports and are not '
  + 'affected. This cannot be undone.';
const DELETE_CONFIRM_LABEL = 'Delete recipe';
const DELETE_BUSY_LABEL = 'Deleting…';

export type RecipesPageProps = Readonly<{
  recipes: readonly TrackRecipe[];
  /** `false` while the first read is in flight or after it failed; the empty state may not be claimed before a read landed. */
  loaded: boolean;
  /** A read failure, told rather than hidden. */
  error: string | null;
  theme: RecipeEditorTheme;
  onWrite: (draft: RecipeDraft, recipeId: string | null) => Promise<RecipeWriteOutcome>;
  onDelete: (recipeId: string) => Promise<void>;
}>;

/** The manage screen: a list, and one recipe open at a time. */
export function RecipesPage({ recipes, loaded, error, theme, onWrite, onDelete }: RecipesPageProps) {
  /** `null` = the list. `''` = a recipe being composed that has no row yet. */
  const [open, setOpen] = useState<string | null>(null);
  /* The row a create resolved to: `onCreated` moves `open` to the new id while `recipes` is still the pre-create list (the invalidate only queues a refetch). */
  const [created, setCreated] = useState<TrackRecipe | null>(null);
  const listed = open === null ? undefined : recipes.find((recipe) => recipe.id === open);
  const current = listed ?? (created !== null && created.id === open ? created : undefined);

  if (open === '') {
    return (
      <RecipeEditor
        recipe={null}
        theme={theme}
        onWrite={(draft) => onWrite(draft, null)}
        onDelete={null}
        onClose={() => setOpen(null)}
        onCreated={(recipe) => { setCreated(recipe); setOpen(recipe.id); }}
      />
    );
  }

  if (current !== undefined) {
    return (
      <RecipeEditor
        /* Keyed by id so switching recipes builds a fresh editor: the editor does not follow its `recipe` prop after mount. */
        key={current.id}
        recipe={current}
        theme={theme}
        onWrite={(draft) => onWrite(draft, current.id)}
        onDelete={() => onDelete(current.id)}
        onClose={() => setOpen(null)}
        onCreated={null}
      />
    );
  }

  return (
    <section className={styles.page} aria-labelledby="nc-recipes-title">
      <header className={styles.head}>
        <h1 className={styles.title} id="nc-recipes-title" data-nc-page-title="" tabIndex={-1}>Recipes</h1>
        <Button type="button" variant="primary" size="sm" label="New recipe" onClick={() => setOpen('')} />
      </header>
      <p className={styles.lede}>
        A recipe is a report you keep: its heading becomes the new track&apos;s summary, and its
        task blocks become that track&apos;s tasks.
      </p>
      {error !== null && <Banner status="warning" title={error} />}
      {recipes.length > 0
        ? (
          <ul className={styles.list}>
            {recipes.map((recipe) => (
              <li key={recipe.id}>
                <button
                  type="button"
                  className={styles.row}
                  onClick={() => setOpen(recipe.id)}
                >
                  <span className={styles.rowTitle}>{recipe.title}</span>
                </button>
              </li>
            ))}
          </ul>
        )
        : loaded && (
          <p className={styles.empty}>
            You have no recipes yet. Anything you save here joins the built-in templates in the
            New track picker.
          </p>
        )}
    </section>
  );
}

/** One recipe: rendered, or open for editing. `current` is seeded from the prop once and thereafter replaced only by what a save resolved to. */
export function RecipeEditor({ recipe, theme, onWrite, onDelete, onClose, onCreated }: Readonly<{
  /** `null` composes a recipe that has no row yet. */
  recipe: TrackRecipe | null;
  theme: RecipeEditorTheme;
  onWrite: (draft: RecipeDraft) => Promise<RecipeWriteOutcome>;
  /** `null` when there is no row to delete yet. */
  onDelete: (() => Promise<void>) | null;
  onClose: () => void;
  /** Called once the composed recipe has a row, so the page can open it. */
  onCreated: ((recipe: TrackRecipe) => void) | null;
}>) {
  const fieldId = useId();
  const [current, setCurrent] = useState<TrackRecipe | null>(recipe);
  const [editing, setEditing] = useState(recipe === null);
  const [title, setTitle] = useState(recipe?.title ?? NEW_RECIPE_TITLE);
  const [body, setBody] = useState(recipe?.body ?? '');
  const [saving, setSaving] = useState(false);
  const [conflict, setConflict] = useState(false);
  const [failure, setFailure] = useState<string | null>(null);
  /* Through the shared confirm/feedback primitive rather than `onDelete().then(onClose)`: a rejected delete has to be said. */
  const deletion = useDeleteConfirm(async () => { await onDelete?.(); }, onClose);

  async function save(): Promise<void> {
    if (saving) return;
    setSaving(true);
    setConflict(false);
    setFailure(null);
    const outcome = await onWrite({ title, body, if_revision: current?.revision ?? null });
    setSaving(false);
    if (outcome.kind === 'conflict') {
      // The draft stays: `title`/`body` are untouched on this path on purpose.
      setConflict(true);
      return;
    }
    if (outcome.kind === 'failed') {
      setFailure(outcome.message);
      return;
    }
    /* Reseed `title`/`body` from the stored row, or the next save would re-send the pre-normalization bytes. */
    setCurrent(outcome.recipe);
    setTitle(outcome.recipe.title);
    setBody(outcome.recipe.body);
    setEditing(false);
    if (current === null) onCreated?.(outcome.recipe);
  }

  return (
    <section className={styles.page} aria-labelledby={`${fieldId}-title`}>
      <header className={styles.head}>
        <h1 className={styles.title} id={`${fieldId}-title`} data-nc-page-title="" tabIndex={-1}>
          {current?.title ?? NEW_RECIPE_TITLE}
        </h1>
        <div className={styles.actions}>
          {editing
            ? (
              <>
                <Button
                  type="button"
                  variant="primary"
                  size="sm"
                  label={saving ? 'Saving…' : 'Save'}
                  isDisabled={saving || title.trim() === ''}
                  onClick={() => { void save(); }}
                />
                <Button
                  type="button"
                  variant="secondary"
                  size="sm"
                  label="Cancel"
                  isDisabled={saving}
                  onClick={() => {
                    if (current === null) { onClose(); return; }
                    setTitle(current.title);
                    setBody(current.body);
                    setConflict(false);
                    setFailure(null);
                    setEditing(false);
                  }}
                />
              </>
            )
            : (
              <>
                <Button type="button" variant="primary" size="sm" label="Edit" onClick={() => setEditing(true)} />
                {onDelete !== null && (
                  <Button
                    type="button"
                    variant="secondary"
                    size="sm"
                    label="Delete"
                    onClick={() => deletion.request(current?.id ?? '')}
                  />
                )}
                <Button type="button" variant="ghost" size="sm" label="All recipes" onClick={onClose} />
              </>
            )}
        </div>
      </header>

      {conflict && <Banner status="warning" title={CONFLICT_NOTICE} />}
      {failure !== null && <Banner status="error" title={failure} />}
      {deletion.feedback.error !== null && <Banner status="error" title={deletion.feedback.error} />}

      {editing
        ? (
          <div className={styles.editor}>
            <TextInput
              label="Recipe title"
              value={title}
              onChange={(next: string) => setTitle(next)}
              isDisabled={saving}
            />
            <p className={styles.bodyLabel} id={`${fieldId}-body-hint`}>
              Body — Markdown. Each <code>neige-block</code> task fence becomes one task.
              {' '}
              Format guide: <code>docs/recipe-body-format.md</code> in the repository.
            </p>
            <div className={styles.code} data-nc-recipe-body="">
              <RecipeBodyEditor
                id={`${fieldId}-body`}
                value={body}
                theme={theme}
                label={BODY_FIELD_LABEL}
                onChange={(next: string) => setBody(next)}
              />
            </div>
          </div>
        )
        : (
          <article className={`calm-prose ${styles.rendered}`} data-nc-recipe-rendered="">
            <ProseBlock markdown={current?.body ?? ''} blockId={null} />
          </article>
        )}

      {onDelete !== null && (
        <ConfirmDialog
          open={deletion.open}
          title={DELETE_TITLE}
          description={DELETE_DESCRIPTION}
          confirmLabel={DELETE_CONFIRM_LABEL}
          confirmBusyLabel={DELETE_BUSY_LABEL}
          confirmState={deletion.pending ? 'busy' : 'ready'}
          onCancel={deletion.cancel}
          onConfirm={deletion.confirm}
        />
      )}
    </section>
  );
}
