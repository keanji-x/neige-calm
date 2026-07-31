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
  number: number | null;
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

function collectSectionLabels(node: unknown, labels: string[]): void {
  if (typeof node !== 'object' || node === null) return;
  const candidate = node as MarkdownNode;
  if (
    candidate.type === 'heading' &&
    (candidate.depth === 1 || candidate.depth === 2)
  ) {
    labels.push(nodeText(candidate));
  }
  if (!Array.isArray(candidate.children)) return;
  for (const child of candidate.children) collectSectionLabels(child, labels);
}

function blockLabel(block: ReportBlock): string {
  const payload = block.payload as Record<string, unknown>;
  for (const key of ['symbol', 'src', 'caption', 'title']) {
    const value = payload[key];
    if (typeof value === 'string') return value;
  }
  return block.kind;
}

export function reportHeadingId(blockId: string, index: number): string {
  return `${blockId}-h${index + 1}`;
}

/** Derive report navigation from persisted blocks without touching React. */
export function deriveOutline(
  blocks: readonly ReportBlock[] | undefined,
): ReportOutlineItem[] {
  if (!blocks) return [];

  const outline: ReportOutlineItem[] = [];
  let sectionNumber = 0;
  for (const block of blocks) {
    if (block.kind === 'prose') {
      const markdown = (block.payload as Record<string, unknown>).markdown;
      if (typeof markdown !== 'string') continue;
      const root = fromMarkdown(markdown);
      const labels: string[] = [];
      collectSectionLabels(root, labels);
      for (const [index, label] of labels.entries()) {
        sectionNumber += 1;
        outline.push({
          blockId: reportHeadingId(block.id, index),
          label,
          number: sectionNumber,
          children: [],
        });
      }
      continue;
    }

    const parent = outline.at(-1);
    if (!parent || parent.number === null) {
      outline.push({
        blockId: block.id,
        label: blockLabel(block),
        number: null,
        children: [],
      });
      continue;
    }
    parent.children.push({
      blockId: block.id,
      kind: block.kind,
      label: blockLabel(block),
    });
  }
  return outline;
}
