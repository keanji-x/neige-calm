import type { ApiTransportPort } from '../../../../core/api/types.ts';
import type { UnauthorizedChannel } from '../../../../core/api/unauthorized.ts';
import { readWriteFailure } from '../../../../core/domain/failure-class.ts';
import { RECIPE_CREATE_FAILURES, RECIPE_CREATE_TEXT, RECIPE_SAVE_FAILURES, RECIPE_SAVE_TEXT } from '../../../../core/domain/track.ts';
import { RecipesPage, type RecipeDraft, type RecipeWriteOutcome } from '../../features/report/recipe/public.tsx';
import { useTrackRecipeMutations, useTrackRecipes } from '../providers/queries.ts';
import { useTheme } from '../theme/public.tsx';

/**
 * `/recipes`. A failed write is read through its table (#2131): a stale save keeps the editor and every character of the
 * draft; a create whose outcome is unknown is not sent again from the same draft, because it carries no key.
 */
export function RecipesRoute({ transport, unauthorized }: { transport: ApiTransportPort; unauthorized: UnauthorizedChannel }) {
  const recipes = useTrackRecipes(transport, unauthorized);
  const mutations = useTrackRecipeMutations(transport, unauthorized);
  const { resolved } = useTheme();

  const write = async (draft: RecipeDraft, recipeId: string | null): Promise<RecipeWriteOutcome> => {
    if (draft.if_revision === null || recipeId === null) {
      try {
        return { kind: 'saved', recipe: await mutations.create({ title: draft.title, body: draft.body }) };
      } catch (failure: unknown) {
        const reading = readWriteFailure(failure, RECIPE_CREATE_FAILURES, RECIPE_CREATE_TEXT);
        return { kind: reading.is === 'unknown' ? 'unconfirmed' : 'failed', message: reading.text };
      }
    }
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
