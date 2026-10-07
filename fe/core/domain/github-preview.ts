import { z } from 'zod';
import type { ApiAbortSignal, ApiOperation } from '../api/types.js';
import type { GitHubReference } from './issue-url.js';

export const githubPreviewSchema = z.object({
  kind: z.enum(['issue', 'pull']), number: z.number().int().positive(), title: z.string(),
  state: z.enum(['open', 'closed', 'merged', 'draft']), author: z.string(), labels: z.array(z.string()),
  excerpt: z.string(), changes: z.object({
    additions: z.number().int().nonnegative(), deletions: z.number().int().nonnegative(),
    changed_files: z.number().int().nonnegative(),
  }).nullable(),
});
export type GitHubPreview = z.infer<typeof githubPreviewSchema>;
export type GitHubPreviewPort = Readonly<{
  read(target: GitHubReference, signal: ApiAbortSignal): Promise<GitHubPreview>;
}>;
export function githubPreviewOperation(target: GitHubReference, signal?: ApiAbortSignal): ApiOperation<GitHubPreview> {
  return {
    method: 'GET',
    path: `/api/github/preview?owner=${encodeURIComponent(target.owner)}&repo=${encodeURIComponent(target.name)}&kind=${target.kind}&number=${target.number}`,
    responseSchema: githubPreviewSchema,
    ...(signal === undefined ? {} : { signal }),
  };
}
