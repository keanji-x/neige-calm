import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { NotSentError, readWriteFailure } from '../../../../core/domain/failure-class.ts';
import {
  readKeyedCreateFailure, RECIPE_CREATE_FAILURES, RECIPE_CREATE_TEXT, RECIPE_SAVE_FAILURES, RECIPE_SAVE_TEXT,
} from '../../../../core/domain/track.ts';
import { RecipesPage, type RecipeDraft, type RecipeWriteOutcome } from '../../features/report/recipe/public.tsx';
import { useKeyedIntent } from '../providers/idempotency-key.ts';
import { useTrackRecipeMutations, useTrackRecipes } from '../providers/queries.ts';
import { useTheme } from '../theme/public.tsx';

type RecipeCreateBody = Readonly<{ title: string; body: string }>;

const sameRecipeCreate = (held: RecipeCreateBody, next: RecipeCreateBody): boolean =>
  held.title === next.title && held.body === next.body;

/**
 * `/recipes`. A failed write is read through its table (#2131): a stale save keeps the editor and every character of the
 * draft; a create whose outcome is unknown keeps its `Idempotency-Key`, so its Try again is answered with the recipe the
 * first attempt made rather than making a second one.
 */
export function RecipesRoute({ transport, unauthorized }: { transport: ApiTransportPort; unauthorized: UnauthorizedChannel }) {
  const recipes = useTrackRecipes(transport, unauthorized);
  const mutations = useTrackRecipeMutations(transport, unauthorized);
  const { resolved } = useTheme();
  /* One new recipe's create is one key: the held request (key and body) goes again on Try again; a final outcome
   * releases it, and another title or body is a new intent under a new key. */
  const createIntent = useKeyedIntent<RecipeCreateBody, RecipeCreateBody>(sameRecipeCreate);

  const create = async (draft: RecipeCreateBody): Promise<RecipeWriteOutcome> => {
    const resent = createIntent.held !== null && sameRecipeCreate(createIntent.held.draft, draft);
    const request = createIntent.request(draft, () => draft);
    try {
      const recipe = await mutations.create(request.body, request.key);
      createIntent.release(request);
      return { kind: 'saved', recipe };
    } catch (failure: unknown) {
      const reading = readKeyedCreateFailure(failure, RECIPE_CREATE_FAILURES, RECIPE_CREATE_TEXT);
      /* A resend that sent nothing leaves the earlier unknown outcome, and its key, standing. */
      if (reading.is === 'unknown' || (resent && failure instanceof NotSentError)) {
        return { kind: 'unconfirmed', message: RECIPE_CREATE_TEXT.unknown };
      }
      createIntent.release(request);
      return { kind: 'failed', message: reading.text };
    }
  };

  const write = async (draft: RecipeDraft, recipeId: string | null): Promise<RecipeWriteOutcome> => {
    if (draft.if_revision === null || recipeId === null) return create({ title: draft.title, body: draft.body });
    try {
      return { kind: 'saved', recipe: await mutations.save(recipeId, { title: draft.title, body: draft.body, if_revision: draft.if_revision }) };
    } catch (failure: unknown) {
      const reading = readWriteFailure(failure, RECIPE_SAVE_FAILURES, RECIPE_SAVE_TEXT);
      return reading.is === 'stale' ? { kind: 'conflict' } : { kind: 'failed', message: reading.text };
    }
  };

  return (
    <RecipesPage
      recipes={recipes.recipes}
      refreshing={recipes.refreshing}
      onRetry={recipes.refetch}
      loaded={recipes.loaded}
      error={recipes.error}
      theme={resolved}
      onWrite={write}
      onDelete={mutations.remove}
    />
  );
}
