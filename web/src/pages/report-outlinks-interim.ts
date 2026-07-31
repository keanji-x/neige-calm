import type { ReportBlock } from '../cards/builtins/wave-report';

// Temporary client-only bridge for issue #975. Delete this entire module once
// the `/links` endpoint lands; server-derived outlinks will replace it.

export interface InterimReportOutlink {
  waveId: string;
  blockId?: string;
}

const NEIGE_WAVE_URL =
  /neige:\/\/wave\/([A-Za-z0-9._~%-]+)(?:#([A-Za-z0-9._~%-]+))?/g;

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

    for (const match of markdown.matchAll(NEIGE_WAVE_URL)) {
      const waveId = match[1];
      if (seen.has(waveId)) continue;
      seen.add(waveId);
      outlinks.push({ waveId, blockId: match[2] });
    }
  }
  return outlinks;
}
