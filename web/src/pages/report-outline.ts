import { fromMarkdown } from 'mdast-util-from-markdown';
import type { ReportBlock } from '../cards/builtins/wave-report';

export interface ReportOutlineChild {
  blockId: string;
  kind: string;
  label: string;
}

export interface ReportOutlineItem {
  blockId: string;
  label: string;
  number: number;
  children: ReportOutlineChild[];
}

type MarkdownNode = {
  type?: unknown;
  depth?: unknown;
  value?: unknown;
  alt?: unknown;
  children?: unknown;
};

function nodeText(node: unknown): string {
  if (typeof node !== 'object' || node === null) return '';
  const candidate = node as MarkdownNode;
  if (typeof candidate.value === 'string') return candidate.value;
  if (typeof candidate.alt === 'string') return candidate.alt;
  if (!Array.isArray(candidate.children)) return '';
  return candidate.children.map(nodeText).join('');
}

function collectH2Labels(node: unknown, labels: string[]): void {
  if (typeof node !== 'object' || node === null) return;
  const candidate = node as MarkdownNode;
  if (candidate.type === 'heading' && candidate.depth === 2) {
    labels.push(nodeText(candidate));
  }
  if (!Array.isArray(candidate.children)) return;
  for (const child of candidate.children) collectH2Labels(child, labels);
}

function blockLabel(block: ReportBlock): string {
  const payload = block.payload as Record<string, unknown>;
  for (const key of ['symbol', 'src', 'caption', 'title']) {
    const value = payload[key];
    if (typeof value === 'string') return value;
  }
  return block.kind;
}

/** Derive report navigation from persisted blocks without touching React. */
export function deriveOutline(
  blocks: readonly ReportBlock[] | undefined,
): ReportOutlineItem[] {
  if (!blocks) return [];

  const outline: ReportOutlineItem[] = [];
  for (const block of blocks) {
    if (block.kind === 'prose') {
      const markdown = (block.payload as Record<string, unknown>).markdown;
      if (typeof markdown !== 'string') continue;
      const root = fromMarkdown(markdown);
      const labels: string[] = [];
      collectH2Labels(root, labels);
      for (const label of labels) {
        outline.push({
          blockId: block.id,
          label,
          number: outline.length + 1,
          children: [],
        });
      }
      continue;
    }

    const parent = outline.at(-1);
    if (!parent) continue;
    parent.children.push({
      blockId: block.id,
      kind: block.kind,
      label: blockLabel(block),
    });
  }
  return outline;
}
