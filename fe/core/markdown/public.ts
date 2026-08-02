import { fromMarkdown } from 'mdast-util-from-markdown';
import { gfmFromMarkdown } from 'mdast-util-gfm';
import { gfm } from 'micromark-extension-gfm';

export type MarkdownDepth = 1 | 2 | 3 | 4 | 5 | 6;

export type NormalizedText = Readonly<{ type: 'text'; value: string }>;
export type NormalizedInlineCode = Readonly<{ type: 'inlineCode'; value: string }>;
export type NormalizedImage = Readonly<{ type: 'image'; alt: string; destination: string; title: string | null }>;
export type NormalizedLink = Readonly<{
  type: 'link';
  destination: string;
  title: string | null;
  children: readonly NormalizedInline[];
}>;
export type NormalizedDelete = Readonly<{ type: 'delete'; children: readonly NormalizedInline[] }>;
export type NormalizedEmphasis = Readonly<{ type: 'emphasis'; children: readonly NormalizedInline[] }>;
export type NormalizedStrong = Readonly<{ type: 'strong'; children: readonly NormalizedInline[] }>;
export type NormalizedBreak = Readonly<{ type: 'break' }>;
export type NormalizedHtml = Readonly<{ type: 'html'; value: string }>;
export type NormalizedInline =
  | NormalizedText
  | NormalizedInlineCode
  | NormalizedImage
  | NormalizedLink
  | NormalizedDelete
  | NormalizedEmphasis
  | NormalizedStrong
  | NormalizedBreak
  | NormalizedHtml;

export type NormalizedHeading = Readonly<{
  type: 'heading';
  depth: MarkdownDepth;
  children: readonly NormalizedInline[];
}>;
export type NormalizedParagraph = Readonly<{ type: 'paragraph'; children: readonly NormalizedInline[] }>;
export type NormalizedCode = Readonly<{ type: 'code'; language: string | null; meta: string | null; value: string }>;
export type NormalizedBlockquote = Readonly<{ type: 'blockquote'; children: readonly NormalizedBlock[] }>;
export type NormalizedListItem = Readonly<{
  type: 'listItem';
  checked: boolean | null;
  children: readonly NormalizedBlock[];
}>;
export type NormalizedList = Readonly<{
  type: 'list';
  ordered: boolean;
  start: number | null;
  spread: boolean;
  children: readonly NormalizedListItem[];
}>;
export type NormalizedTableCell = Readonly<{ type: 'tableCell'; children: readonly NormalizedInline[] }>;
export type NormalizedTableRow = Readonly<{ type: 'tableRow'; children: readonly NormalizedTableCell[] }>;
export type NormalizedTable = Readonly<{
  type: 'table';
  align: readonly ('left' | 'right' | 'center' | null)[];
  children: readonly NormalizedTableRow[];
}>;
export type NormalizedThematicBreak = Readonly<{ type: 'thematicBreak' }>;
export type NormalizedBlock =
  | NormalizedHeading
  | NormalizedParagraph
  | NormalizedCode
  | NormalizedBlockquote
  | NormalizedList
  | NormalizedHtml
  | NormalizedTable
  | NormalizedThematicBreak;

export type MarkdownDiagnostic = Readonly<{
  kind: 'malformed' | 'unsafe-raw-html' | 'limit-exceeded';
  message: string;
  line: number;
}>;

export type NormalizedMarkdownAst = Readonly<{
  type: 'root';
  dialect: 'gfm';
  children: readonly NormalizedBlock[];
  diagnostics: readonly MarkdownDiagnostic[];
}>;

export type MarkdownParseFailure = Readonly<{ kind: 'markdown-parse'; message: string; cause?: unknown }>;
export type MarkdownParseResult =
  | Readonly<{ status: 'ready'; value: NormalizedMarkdownAst }>
  | Readonly<{ status: 'failed'; error: MarkdownParseFailure }>;

/** The file viewer omits headings whose normalized label is empty, matching its legacy TOC. */
export type TextPolicy = 'heading-label' | 'non-empty-heading-label';

export type HeadingIdInput<Context> = Readonly<{
  heading: NormalizedHeading;
  globalOrdinal: number;
  localOrdinal: number;
  context: Context;
}>;
export type HeadingIdPolicy<Context> = Readonly<{
  version: 1;
  createId(input: HeadingIdInput<Context>): string;
}>;
export type HeadingOutline = Readonly<{
  depth: MarkdownDepth;
  id: string;
  text: string;
  globalOrdinal: number;
  localOrdinal: number;
}>;
export type OutlineInput<Context> = Readonly<{ context: Context; ast: NormalizedMarkdownAst }>;
export type ExtractOutlineOptions<Context> = Readonly<{
  maxDepth: MarkdownDepth;
  headingId: HeadingIdPolicy<Context>;
  textPolicy: TextPolicy;
}>;

export const REPORT_MAX_DEPTH = 2 as const;
export const FILE_VIEWER_MAX_DEPTH = 4 as const;
export const reportHeadingIdPolicy: HeadingIdPolicy<Readonly<{ blockId: string }>> = Object.freeze({
  version: 1,
  createId: ({ context, localOrdinal }) => `${context.blockId}-h${localOrdinal + 1}`,
});
export const fileViewerHeadingIdPolicy: HeadingIdPolicy<undefined> = Object.freeze({
  version: 1,
  createId: ({ globalOrdinal }) => `md-h-${globalOrdinal}`,
});

export type SanitizeAstPolicy = Readonly<{ rawHtml: 'drop' }>;
export type SafeInline =
  | NormalizedText
  | NormalizedInlineCode
  | NormalizedImage
  | NormalizedBreak
  | Readonly<{ type: 'link'; destination: string; title: string | null; children: readonly SafeInline[] }>
  | Readonly<{ type: 'delete' | 'emphasis' | 'strong'; children: readonly SafeInline[] }>;
export type SafeBlock =
  | Readonly<{ type: 'heading'; depth: MarkdownDepth; children: readonly SafeInline[] }>
  | Readonly<{ type: 'paragraph'; children: readonly SafeInline[] }>
  | NormalizedCode
  | Readonly<{ type: 'blockquote'; children: readonly SafeBlock[] }>
  | Readonly<{
    type: 'list'; ordered: boolean; start: number | null; spread: boolean;
    children: readonly Readonly<{ type: 'listItem'; checked: boolean | null; children: readonly SafeBlock[] }>[];
  }>
  | Readonly<{
    type: 'table'; align: readonly ('left' | 'right' | 'center' | null)[];
    children: readonly Readonly<{
      type: 'tableRow';
      children: readonly Readonly<{ type: 'tableCell'; children: readonly SafeInline[] }>[];
    }>[];
  }>
  | NormalizedThematicBreak;
export type SafeMarkdownAst = Readonly<{
  type: 'root';
  dialect: 'gfm';
  children: readonly SafeBlock[];
  diagnostics: readonly MarkdownDiagnostic[];
}>;

type MdNode = Readonly<{
  type: string;
  value?: string;
  alt?: string | null;
  url?: string;
  title?: string | null;
  lang?: string | null;
  meta?: string | null;
  depth?: number;
  ordered?: boolean;
  start?: number | null;
  spread?: boolean;
  checked?: boolean | null;
  align?: readonly ('left' | 'right' | 'center' | null)[];
  children?: readonly MdNode[];
}>;

function normalizeInline(node: MdNode): NormalizedInline | null {
  switch (node.type) {
    case 'text': return { type: 'text', value: node.value ?? '' };
    case 'inlineCode': return { type: 'inlineCode', value: node.value ?? '' };
    case 'image': return { type: 'image', alt: node.alt ?? '', destination: node.url ?? '', title: node.title ?? null };
    case 'link': return { type: 'link', destination: node.url ?? '', title: node.title ?? null, children: normalizeInlines(node.children) };
    case 'delete': return { type: 'delete', children: normalizeInlines(node.children) };
    case 'emphasis': return { type: 'emphasis', children: normalizeInlines(node.children) };
    case 'strong': return { type: 'strong', children: normalizeInlines(node.children) };
    case 'break': return { type: 'break' };
    case 'html': return { type: 'html', value: node.value ?? '' };
    default: return null;
  }
}

function normalizeInlines(nodes: readonly MdNode[] | undefined): readonly NormalizedInline[] {
  return (nodes ?? []).flatMap((node) => {
    const normalized = normalizeInline(node);
    return normalized === null ? [] : [normalized];
  });
}

function normalizeBlock(node: MdNode): NormalizedBlock | null {
  switch (node.type) {
    case 'heading':
      if (node.depth === undefined || node.depth < 1 || node.depth > 6) return null;
      return { type: 'heading', depth: node.depth as MarkdownDepth, children: normalizeInlines(node.children) };
    case 'paragraph': return { type: 'paragraph', children: normalizeInlines(node.children) };
    case 'code': return { type: 'code', language: node.lang ?? null, meta: node.meta ?? null, value: node.value ?? '' };
    case 'blockquote': return { type: 'blockquote', children: normalizeBlocks(node.children) };
    case 'list': return {
      type: 'list', ordered: node.ordered ?? false, start: node.start ?? null, spread: node.spread ?? false,
      children: (node.children ?? []).flatMap((child) => {
        const item = normalizeListItem(child);
        return item === null ? [] : [item];
      }),
    };
    case 'html': return { type: 'html', value: node.value ?? '' };
    case 'table': return {
      type: 'table', align: node.align ?? [],
      children: (node.children ?? []).map((row) => ({
        type: 'tableRow',
        children: (row.children ?? []).map((cell) => ({ type: 'tableCell', children: normalizeInlines(cell.children) })),
      })),
    };
    case 'thematicBreak': return { type: 'thematicBreak' };
    default: return null;
  }
}

function normalizeListItem(node: MdNode): NormalizedListItem | null {
  if (node.type !== 'listItem') return null;
  return { type: 'listItem', checked: node.checked ?? null, children: normalizeBlocks(node.children) };
}

function normalizeBlocks(nodes: readonly MdNode[] | undefined): readonly NormalizedBlock[] {
  return (nodes ?? []).flatMap((node) => {
    const normalized = normalizeBlock(node);
    return normalized === null ? [] : [normalized];
  });
}

function diagnosticsFor(markdown: string, root: MdNode): readonly MarkdownDiagnostic[] {
  const diagnostics: MarkdownDiagnostic[] = [];
  const lines = markdown.replace(/\r\n?/g, '\n').split('\n');
  let openFence: Readonly<{ marker: string; size: number; line: number }> | null = null;
  for (const [index, line] of lines.entries()) {
    const fence = /^ {0,3}(`{3,}|~{3,})/.exec(line);
    if (openFence !== null) {
      const run = fence?.[1] ?? '';
      if (run[0] === openFence.marker && run.length >= openFence.size) openFence = null;
      continue;
    }
    if (fence !== null) {
      const run = fence[1] ?? '';
      openFence = { marker: run[0] ?? '`', size: run.length, line: index + 1 };
      continue;
    }
    const atx = /^ {0,3}(#{7,})(?:\s+|$)/.exec(line);
    if (atx !== null) diagnostics.push({ kind: 'malformed', message: 'ATX heading depth exceeds six', line: index + 1 });
    const quoteDepth = /^(?:>\s*)+/.exec(line)?.[0].split('>').length ?? 1;
    if (quoteDepth - 1 > 64) diagnostics.push({ kind: 'limit-exceeded', message: 'Block nesting exceeds 64 levels', line: index + 1 });
    const possibleDelimiter = lines[index + 1] ?? '';
    if (line.includes('|') && possibleDelimiter.includes('|') && possibleDelimiter.includes('-')) {
      const delimiterCells = possibleDelimiter.replace(/^\s*\||\|\s*$/g, '').split('|');
      const validDelimiter = delimiterCells.length > 0 && delimiterCells.every((cell) => /^\s*:?-+:?\s*$/.test(cell));
      if (!validDelimiter) diagnostics.push({ kind: 'malformed', message: 'Malformed GFM table delimiter', line: index + 2 });
    }
  }
  if (openFence !== null) diagnostics.push({ kind: 'malformed', message: 'Unclosed fenced code block', line: openFence.line });
  const visitHtml = (node: MdNode): void => {
    if (node.type === 'html') {
      const line = lines.findIndex((candidate) => candidate.includes(node.value ?? '')) + 1;
      diagnostics.push({ kind: 'unsafe-raw-html', message: 'Raw HTML requires sanitization', line: Math.max(1, line) });
    }
    for (const child of node.children ?? []) visitHtml(child);
  };
  visitHtml(root);
  return diagnostics;
}

export function parse(markdown: string): MarkdownParseResult {
  try {
    const root = fromMarkdown(markdown, { extensions: [gfm()], mdastExtensions: [gfmFromMarkdown()] }) as MdNode;
    return {
      status: 'ready',
      value: { type: 'root', dialect: 'gfm', children: normalizeBlocks(root.children), diagnostics: diagnosticsFor(markdown, root) },
    };
  } catch (cause) {
    return { status: 'failed', error: { kind: 'markdown-parse', message: 'Markdown normalization failed', cause } };
  }
}

function inlineText(nodes: readonly NormalizedInline[], policy: TextPolicy): string {
  void policy;
  return nodes.map((node) => {
    if (node.type === 'text' || node.type === 'inlineCode') return node.value;
    if (node.type === 'image') return node.alt;
    if (node.type === 'break') return ' ';
    if (node.type === 'html') return '';
    return inlineText(node.children, policy);
  }).join('').replace(/\s+/g, ' ').trim();
}

export function extractOutline<Context>(
  inputs: readonly OutlineInput<Context>[],
  options: ExtractOutlineOptions<Context>,
): HeadingOutline[] {
  const outline: HeadingOutline[] = [];
  let globalOrdinal = 0;
  for (const { ast, context } of inputs) {
    let localOrdinal = 0;
    for (const node of ast.children) {
      if (node.type !== 'heading' || node.depth > options.maxDepth) continue;
      const text = inlineText(node.children, options.textPolicy);
      if (options.textPolicy === 'non-empty-heading-label' && text.length === 0) continue;
      const input = { heading: node, globalOrdinal, localOrdinal, context };
      outline.push({
        depth: node.depth, id: options.headingId.createId(input), text, globalOrdinal, localOrdinal,
      });
      globalOrdinal += 1;
      localOrdinal += 1;
    }
  }
  return outline;
}

function sanitizeInline(node: NormalizedInline): SafeInline | null {
  if (node.type === 'html') return null;
  if (node.type === 'link' || node.type === 'delete' || node.type === 'emphasis' || node.type === 'strong') {
    return { ...node, children: node.children.flatMap((child) => {
      const safe = sanitizeInline(child);
      return safe === null ? [] : [safe];
    }) };
  }
  return node;
}

function sanitizeBlock(node: NormalizedBlock): SafeBlock | null {
  if (node.type === 'html') return null;
  if (node.type === 'blockquote') {
    return { ...node, children: node.children.flatMap((child) => {
      const safe = sanitizeBlock(child);
      return safe === null ? [] : [safe];
    }) };
  }
  if (node.type === 'list') return {
    ...node,
    children: node.children.map((item) => ({ ...item, children: item.children.flatMap((child) => {
      const safe = sanitizeBlock(child);
      return safe === null ? [] : [safe];
    }) })),
  };
  if (node.type === 'heading' || node.type === 'paragraph') return {
    ...node,
    children: node.children.flatMap((child) => {
      const safe = sanitizeInline(child);
      return safe === null ? [] : [safe];
    }),
  };
  if (node.type === 'table') return {
    ...node,
    children: node.children.map((row) => ({ ...row, children: row.children.map((cell) => ({
      ...cell,
      children: cell.children.flatMap((child) => {
        const safe = sanitizeInline(child);
        return safe === null ? [] : [safe];
      }),
    })) })),
  };
  return node;
}

/** This AST policy removes raw HTML nodes; it is not an HTML renderer allowlist. */
export function sanitizeAstPolicy(ast: NormalizedMarkdownAst, policy: SanitizeAstPolicy): SafeMarkdownAst {
  void policy;
  return {
    ...ast,
    children: ast.children.flatMap((node) => {
      const safe = sanitizeBlock(node);
      return safe === null ? [] : [safe];
    }),
  };
}
