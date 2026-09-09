import { z } from 'zod';
import type { ApiAbortSignal, ApiOperation } from '../api/types.js';
import { compiledReportPayloadSchema, readReportPayload } from './report.js';

export const recipePreviewSchema = z.object({
  id: z.string(), revision: z.number().int(), payload: compiledReportPayloadSchema,
}).transform((value, ctx) => {
  const report = readReportPayload(value.payload);
  if (report === null) {
    ctx.addIssue({ code: 'custom', path: ['payload'], message: 'Preview report is missing' });
    return z.NEVER;
  }
  return { id: value.id, revision: value.revision, report };
});

export type RecipePreview = z.infer<typeof recipePreviewSchema>;

export function recipePreviewOperation(id: string, revision: number, signal: ApiAbortSignal): ApiOperation<RecipePreview> {
  return { method: 'GET', path: `/api/track-recipes/${encodeURIComponent(id)}/preview?if_revision=${revision}`,
    signal, responseSchema: recipePreviewSchema };
}
