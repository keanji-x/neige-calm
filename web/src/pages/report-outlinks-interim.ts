import type { ReportBlock } from '../cards/builtins/wave-report';
import { fromMarkdown } from 'mdast-util-from-markdown';
import { BLOCK_ID_PATTERN } from './report-link-ids';

// Temporary client-only bridge for issue #975. Delete this entire module once
// the `/links` endpoint lands; server-derived outlinks will replace it.

export interface InterimReportOutlink {
  waveId: string;
  blockId?: string;
}

const NEIGE_WAVE_URL =
  /^neige:\/\/wave\/([A-Za-z0-9._~%-]+)(?:#([A-Za-z0-9._~%-]+))?$/;

type MarkdownNode = {
  type?: unknown;
  url?: unknown;
  children?: unknown;
};

function collectLinkUrls(node: unknown, urls: string[]): void {
  if (typeof node !== 'object' || node === null) return;
  const candidate = node as MarkdownNode;
  if (candidate.type === 'link' && typeof candidate.url === 'string') {
    urls.push(candidate.url);
  }
  if (!Array.isArray(candidate.children)) return;
  for (const child of candidate.children) collectLinkUrls(child, urls);
}

export function deriveInterimReportOutlinks(
  blocks: readonly ReportBlock[] | undefined,
): InterimReportOutlink[] {
  if (!blocks) return [];

  const seen = new Set<string>();
  const outlinks: InterimReportOutlink[] = [];
  for (const block of blocks) {
    if (block.kind !== 'prose') continue;
    const markdown = (block.payload as Record<string, unknown>).markdown;
    if (typeof markdown !== 'string') continue;

    const urls: string[] = [];
    collectLinkUrls(fromMarkdown(markdown), urls);
    for (const url of urls) {
      const match = url.match(NEIGE_WAVE_URL);
      if (!match) continue;
      const waveId = match[1];
      if (seen.has(waveId)) continue;
      seen.add(waveId);
      const blockId = match[2];
      outlinks.push({
        waveId,
        blockId:
          blockId != null && BLOCK_ID_PATTERN.test(blockId)
            ? blockId
            : undefined,
      });
    }
  }
  return outlinks;
}
