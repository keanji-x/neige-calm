import type { ApiTransportResponse } from '../../../fe/core/api/types.ts';

export const PORTFOLIO_METADATA_PATH = '.neige-portfolio/metadata.json';

/** The current workspace-file route reports a missing path as this exact 400,
 * while 404 means the Track is missing. Never treat all read failures as empty. */
export function metadataIsMissing(response: ApiTransportResponse, workspace: string): boolean {
  if (response.status !== 400 || typeof response.body !== 'object' || response.body === null) return false;
  const body = response.body as { code?: unknown; error?: unknown };
  const expected = `path ${workspace.replace(/\/+$/, '')}/${PORTFOLIO_METADATA_PATH} not found`;
  return body.code === 'bad_request' && body.error === expected;
}
