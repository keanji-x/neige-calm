// Recipes: the user's own saved starting points for a new track (#1292 S4).
//
// A recipe is a saved report — a title, which doubles as the created track's
// report summary, and a body whose `neige-block` fences *are* its tasks. This
// module is the list of them and the editor for one.
//
// ## Why it lives in `features/report/`
//
// The "rendered body" half of this screen is `ProseBlock` from
// `../document/public.tsx` — `core/markdown`'s `parse` plus
// `sanitizeAstPolicy`, the exact picture a recipe view wants. Two rules
// bracket the placement and together they leave one option:
//
//   * `features-no-cross-domain` (`.dependency-cruiser.cjs`) forbids any other
//     `features/*` from importing `features/report/*`;
//   * INV-DUP-004 / INV-DUP-005 make `core/markdown` the single Markdown path
//     — `systems/fs-viewers/public.tsx` records that `react-markdown` was
//     deleted on purpose to hold that line.
//
// So a recipe editor sited anywhere else would have had to grow a second
// renderer, breaking the second rule to satisfy the first. Being here it
// simply imports the function.
//
// ## Why the editor is raw Markdown and not WYSIWYG
//
// The format is checked where it matters: `validate_recipe_body` rejects a
// malformed body with a 400 at the write boundary, so a bad body never reaches
// the database and the author is told at save time.
//
// This slice originally rested that on a second premise — "the person
// authoring a recipe is the person who knows the fence format". Preview
// disproved it: the owner opened this editor and could not tell what to type.
// The replacement premise is "the author can read our doc", and the carrier is
// `docs/recipe-body-format.md`, referenced from the body label below. No
// in-editor skeleton, hint block or client-side format detection — a client
// that describes the fence format is the second authority on it that the
// paragraph below exists to avoid.
//
// The decisive reason is the other direction. **There is no Markdown
// serializer in this codebase** — `core/markdown` has `parse` and no
// `stringify`, deliberately — so a structured editor would have to invent one,
// and the invented one would be a second authority on what a `neige-block`
// fence looks like next to the kernel's `render_fence`. Raw text has no
// serialize step at all: what the author typed *is* the body.
//
// ## The one rule the whole editor is arranged around
//
// **After a save, the view renders the server's response, never the local
// draft.** The write boundary rewrites what it stores — every fence is
// re-rendered through `render_fence`, tombstones are dropped, the task
// privilege fields are normalized (`routes/track_recipes.rs`). Rendering the
// draft would hide that rewrite until the author next opened the recipe, where
// it would look like their text had been changed behind their back. Rendering
// the response makes the rewrite the thing they see the moment it happens.
//
// That is why `RecipeEditor` holds the loaded row in state and replaces it
// with the value the save resolved to, rather than reading a prop the list
// happens to refetch.
//
// ## Conflicts: 409, and the draft survives
//
// The write is a whole-document `PUT` gated on `if_revision`. There is no
// per-block CAS here and adding one was rejected in design: a recipe's only
// writer is its owner, possibly from two windows, and "partially synced" is
// not a state this thing can be in. The answer to a stale write is therefore
// to tell the author the recipe changed underneath them **and keep every
// character they have typed** — the only thing a conflict costs is a re-read,
// and losing the draft on top of that would make the cheap failure expensive.

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

/**
 * What a write attempt came back as.
 *
 * `conflict` is a state of its own rather than a `failed` with a 409 message
 * because the editor *behaves* differently: it stays in edit mode holding the
 * draft. Collapsing the two would make "keep the draft" depend on parsing an
 * error string.
 */
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

/** The body editable's accessible name. Spelled once and used by both the
 *  visible label and the `contenteditable`'s `aria-label`, which are two
 *  channels for one name and must not drift. */
const BODY_FIELD_LABEL = 'Recipe body, Markdown';

/* The deletion consequence, worded once. Three constants rather than one
   frozen object: a module-level object is module runtime state as far as the
   architecture rule is concerned, and shallow-freezing it would satisfy the
   letter of that rule without changing what it is. Strings have no interior to
   freeze. */
const DELETE_TITLE = 'Delete this recipe?';
const DELETE_DESCRIPTION
  = 'The recipe is removed. Tracks already created from it keep their reports and are not '
  + 'affected. This cannot be undone.';
const DELETE_CONFIRM_LABEL = 'Delete recipe';
/* The in-flight label. Same width discipline as every other destructive
   confirm here: the button swaps text rather than the dialog swapping shape. */
const DELETE_BUSY_LABEL = 'Deleting…';

export type RecipesPageProps = Readonly<{
  recipes: readonly TrackRecipe[];
  /** `false` while the first read is in flight or after it failed. The empty
   *  state is a claim about the server; a read that never landed may not make
   *  it. */
  loaded: boolean;
  /** A read failure, told rather than hidden. */
  error: string | null;
  theme: RecipeEditorTheme;
  onWrite: (draft: RecipeDraft, recipeId: string | null) => Promise<RecipeWriteOutcome>;
  onDelete: (recipeId: string) => Promise<void>;
}>;

/**
 * The manage screen: a list, and one recipe open at a time.
 *
 * List-then-detail on one route rather than two routes, because the detail has
 * no shareable identity worth a URL yet — a recipe is a private authoring
 * artefact, not a thing you send someone a link to. The moment that changes,
 * `open` becomes a route parameter and nothing else here moves.
 */
export function RecipesPage({ recipes, loaded, error, theme, onWrite, onDelete }: RecipesPageProps) {
  /** `null` = the list. `''` = a recipe being composed that has no row yet. */
  const [open, setOpen] = useState<string | null>(null);
  /*
   * The row a create resolved to, held here because the list cannot be relied
   * on to have it. `onCreated` moves `open` to the new id in the same event
   * the create resolved in, and at that moment `recipes` is still the
   * pre-create list — the mutation's invalidate only *queues* a refetch. With
   * nothing but the list to look in, the lookup below would miss, this page
   * would fall through to the list, and the server's response would be
   * discarded unrendered: the one thing the file header says this screen is
   * arranged around, lost on the one path where the rewrite is largest.
   *
   * So the response is what the new editor is seeded from, and the header's
   * rule holds on create by construction rather than by refetch timing. It is
   * also what keeps the reader on the recipe when that refetch *fails*:
   * `trackRecipesQueryOptions` sets `retry: false`, so a failed list read
   * leaves the previous, new-row-less list in the cache.
   */
  const [created, setCreated] = useState<TrackRecipe | null>(null);
  const listed = open === null ? undefined : recipes.find((recipe) => recipe.id === open);
  /* The list wins once it has the row — same row, and then there is one copy
     of it again. `created` is the bridge, not a second authority. */
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
        /* Keyed by id so switching recipes builds a fresh editor. Without it
           the `current` state below would keep the previously opened row —
           the editor deliberately does not follow its `recipe` prop after
           mount, because following it is how a save response gets overwritten
           by the list's refetch. */
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
                {/* A row that navigates is a `<button>`, not a link
                    (INV-A11Y-061). No delete affordance out here: the gesture
                    exists once, inside the recipe it destroys, where the
                    reader can see what they are about to lose. */}
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

/**
 * One recipe: rendered, or open for editing.
 *
 * `current` is the row this editor is showing, seeded from the prop **once**
 * and thereafter replaced only by what a save resolved to. That is the whole
 * post-save contract of this module: see the file header.
 */
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
  /* The delete runs through the shared confirm/feedback primitive rather than
     `onDelete().then(onClose)`: a rejected delete has to be *said*. Closing
     first and swallowing the rejection would leave the reader on a list still
     showing the recipe, with nothing on screen claiming anything went wrong.
     The hook keeps the dialog up and busy while the write is in flight, closes
     it and calls `onClose` only when the delete resolved, and holds the
     failure message otherwise (it also carries INV-CONFIRM-001: closing the
     dialog aborts the request rather than letting it outlive its consent). */
  const deletion = useDeleteConfirm(async () => { await onDelete?.(); }, onClose);

  async function save(): Promise<void> {
    if (saving) return;
    setSaving(true);
    setConflict(false);
    setFailure(null);
    const outcome = await onWrite({ title, body, if_revision: current?.revision ?? null });
    setSaving(false);
    if (outcome.kind === 'conflict') {
      // Everything the author typed stays exactly where it is, and the editor
      // stays open. `title`/`body` are untouched on this path on purpose.
      setConflict(true);
      return;
    }
    if (outcome.kind === 'failed') {
      setFailure(outcome.message);
      return;
    }
    /*
     * The stored row, not the draft. `title` and `body` are reseeded from it
     * too, so a second Edit starts from what the server holds rather than from
     * the text that produced it — otherwise the next save would re-send the
     * pre-normalization bytes and undo the rewrite the reader just saw.
     */
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
            {/* The body is Markdown, and the label says so where the author
                reads it rather than in a tooltip: the fences are the tasks,
                and nothing on this screen is going to guess them for you. */}
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
            {/* The server's body — see the file header. `blockId` is `null`
                because a recipe has no block ids to anchor to: it is not a
                track report and nothing deep-links into it. */}
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
