import { fromMarkdown } from 'mdast-util-from-markdown';
import { gfmFromMarkdown } from 'mdast-util-gfm';
import { gfm } from 'micromark-extension-gfm';

export type MarkdownDepth = 1 | 2 | 3 | 4 | 5 | 6;
export type MarkdownPosition = Readonly<{
  start: Readonly<{ line: number; column: number; offset: number }>;
  end: Readonly<{ offset: number }>;
}>;
type Positioned<T> = Readonly<T & { position: MarkdownPosition }>;

export type NormalizedText = Positioned<{ type: 'text'; value: string }>;
export type NormalizedInlineCode = Positioned<{ type: 'inlineCode'; value: string }>;
export type NormalizedImage = Positioned<{
  type: 'image'; alt: string; destination: string; title: string | null; referenceSource?: string;
}>;
export type NormalizedLink = Positioned<{
  type: 'link';
  destination: string;
  title: string | null;
  children: readonly NormalizedInline[];
  referenceSource?: string;
}>;
export type NormalizedDelete = Positioned<{ type: 'delete'; children: readonly NormalizedInline[] }>;
export type NormalizedEmphasis = Positioned<{ type: 'emphasis'; children: readonly NormalizedInline[] }>;
export type NormalizedStrong = Positioned<{ type: 'strong'; children: readonly NormalizedInline[] }>;
export type NormalizedBreak = Positioned<{ type: 'break' }>;
export type NormalizedHtml = Positioned<{ type: 'html'; value: string }>;
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
  position: MarkdownPosition;
}>;
export type NormalizedParagraph = Positioned<{ type: 'paragraph'; children: readonly NormalizedInline[] }>;
export type NormalizedCode = Positioned<{ type: 'code'; language: string | null; meta: string | null; value: string }>;
export type NormalizedBlockquote = Positioned<{ type: 'blockquote'; children: readonly NormalizedBlock[] }>;
export type NormalizedListItem = Positioned<{
  type: 'listItem';
  checked: boolean | null;
  children: readonly NormalizedBlock[];
}>;
export type NormalizedList = Positioned<{
  type: 'list';
  ordered: boolean;
  start: number | null;
  spread: boolean;
  children: readonly NormalizedListItem[];
}>;
export type NormalizedTableCell = Positioned<{ type: 'tableCell'; children: readonly NormalizedInline[] }>;
export type NormalizedTableRow = Positioned<{ type: 'tableRow'; children: readonly NormalizedTableCell[] }>;
export type NormalizedTable = Positioned<{
  type: 'table';
  align: readonly ('left' | 'right' | 'center' | null)[];
  children: readonly NormalizedTableRow[];
}>;
export type NormalizedThematicBreak = Positioned<{ type: 'thematicBreak' }>;
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
  position: MarkdownPosition;
}>;

export type MarkdownParseFailure = Readonly<{
  kind: 'markdown-parse';
  message: string;
  diagnostics: readonly MarkdownDiagnostic[];
  cause?: unknown;
}>;
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
  position: MarkdownPosition;
}>;
export type OutlineInput<Context> = Readonly<{ context: Context; ast: NormalizedMarkdownAst }>;
export type ExtractOutlineOptions<Context> = Readonly<{
  maxDepth: MarkdownDepth;
  headingId: HeadingIdPolicy<Context>;
  textPolicy: TextPolicy;
  referenceText: 'visible' | 'source';
  traversal: 'recursive' | 'line-level';
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
  | Positioned<{
    type: 'link'; destination: string; title: string | null; children: readonly SafeInline[]; referenceSource?: string;
  }>
  | Positioned<{ type: 'delete' | 'emphasis' | 'strong'; children: readonly SafeInline[] }>;
export type SafeBlock =
  | Positioned<{ type: 'heading'; depth: MarkdownDepth; children: readonly SafeInline[] }>
  | Positioned<{ type: 'paragraph'; children: readonly SafeInline[] }>
  | NormalizedCode
  | Positioned<{ type: 'blockquote'; children: readonly SafeBlock[] }>
  | Positioned<{
    type: 'list'; ordered: boolean; start: number | null; spread: boolean;
    children: readonly Positioned<{ type: 'listItem'; checked: boolean | null; children: readonly SafeBlock[] }>[];
  }>
  | Positioned<{
    type: 'table'; align: readonly ('left' | 'right' | 'center' | null)[];
    children: readonly Positioned<{
      type: 'tableRow';
      children: readonly Positioned<{ type: 'tableCell'; children: readonly SafeInline[] }>[];
    }>[];
  }>
  | NormalizedThematicBreak;
export type SafeMarkdownAst = Readonly<{
  type: 'root';
  dialect: 'gfm';
  children: readonly SafeBlock[];
  diagnostics: readonly MarkdownDiagnostic[];
  position: MarkdownPosition;
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
  identifier?: string;
  label?: string | null;
  referenceType?: 'shortcut' | 'collapsed' | 'full';
  children?: readonly MdNode[];
  position?: Readonly<{
    start: Readonly<{ line: number; column?: number; offset?: number }>;
    end: Readonly<{ offset?: number }>;
  }>;
}>;

function positionOf(node: MdNode): MarkdownPosition {
  return {
    start: {
      line: node.position?.start.line ?? 1,
      column: node.position?.start.column ?? 1,
      offset: node.position?.start.offset ?? 0,
    },
    end: { offset: node.position?.end.offset ?? node.position?.start.offset ?? 0 },
  };
}

type Definition = Readonly<{ destination: string; title: string | null }>;

function normalizeInline(node: MdNode, definitions: ReadonlyMap<string, Definition>): NormalizedInline | null {
  switch (node.type) {
    case 'text': return { type: 'text', value: node.value ?? '', position: positionOf(node) };
    case 'inlineCode': return { type: 'inlineCode', value: node.value ?? '', position: positionOf(node) };
    case 'image': return { type: 'image', alt: node.alt ?? '', destination: node.url ?? '', title: node.title ?? null, position: positionOf(node) };
    case 'link': return { type: 'link', destination: node.url ?? '', title: node.title ?? null, children: normalizeInlines(node.children, definitions), position: positionOf(node) };
    case 'linkReference': {
      const definition = definitions.get(node.identifier ?? '');
      // micromark currently emits undefined references as text; retain this fail-soft guard for parser-extension drift.
      if (definition === undefined) return { type: 'text', value: referenceLiteral(node), position: positionOf(node) };
      return {
        type: 'link', ...definition, children: normalizeInlines(node.children, definitions),
        referenceSource: referenceLiteral(node), position: positionOf(node),
      };
    }
    case 'imageReference': {
      const definition = definitions.get(node.identifier ?? '');
      // micromark currently emits undefined references as text; retain this fail-soft guard for parser-extension drift.
      if (definition === undefined) return { type: 'text', value: `!${referenceLiteral(node)}`, position: positionOf(node) };
      return {
        type: 'image', alt: node.alt ?? '', ...definition, referenceSource: `!${referenceLiteral(node)}`, position: positionOf(node),
      };
    }
    case 'delete': return { type: 'delete', children: normalizeInlines(node.children, definitions), position: positionOf(node) };
    case 'emphasis': return { type: 'emphasis', children: normalizeInlines(node.children, definitions), position: positionOf(node) };
    case 'strong': return { type: 'strong', children: normalizeInlines(node.children, definitions), position: positionOf(node) };
    case 'break': return { type: 'break', position: positionOf(node) };
    case 'html': return { type: 'html', value: node.value ?? '', position: positionOf(node) };
    default: return null;
  }
}

function referenceLiteral(node: MdNode): string {
  const inlineLiteral = (child: MdNode): string => child.value ?? child.alt
    ?? child.children?.map(inlineLiteral).join('') ?? '';
  const label = node.alt ?? node.children?.map(inlineLiteral).join('') ?? '';
  if (node.referenceType === 'shortcut') return `[${label}]`;
  if (node.referenceType === 'collapsed') return `[${label}][]`;
  return `[${label}][${node.label ?? node.identifier ?? ''}]`;
}

function normalizeInlines(nodes: readonly MdNode[] | undefined, definitions: ReadonlyMap<string, Definition>): readonly NormalizedInline[] {
  return (nodes ?? []).flatMap((node) => {
    const normalized = normalizeInline(node, definitions);
    return normalized === null ? [] : [normalized];
  });
}

function normalizeBlock(node: MdNode, definitions: ReadonlyMap<string, Definition>): NormalizedBlock | null {
  switch (node.type) {
    case 'heading':
      return { type: 'heading', depth: node.depth as MarkdownDepth, children: normalizeInlines(node.children, definitions), position: positionOf(node) };
    case 'paragraph': return { type: 'paragraph', children: normalizeInlines(node.children, definitions), position: positionOf(node) };
    case 'code': return { type: 'code', language: node.lang ?? null, meta: node.meta ?? null, value: node.value ?? '', position: positionOf(node) };
    case 'blockquote': return { type: 'blockquote', children: normalizeBlocks(node.children, definitions), position: positionOf(node) };
    case 'list': return {
      type: 'list', ordered: node.ordered ?? false, start: node.start ?? null, spread: node.spread ?? false,
      position: positionOf(node), children: (node.children ?? []).flatMap((child) => {
        const item = normalizeListItem(child, definitions);
        return item === null ? [] : [item];
      }),
    };
    case 'html': return { type: 'html', value: node.value ?? '', position: positionOf(node) };
    case 'table': return {
      type: 'table', align: node.align ?? [],
      position: positionOf(node), children: (node.children ?? []).map((row) => ({
        type: 'tableRow',
        position: positionOf(row), children: (row.children ?? []).map((cell) => ({ type: 'tableCell', children: normalizeInlines(cell.children, definitions), position: positionOf(cell) })),
      })),
    };
    case 'thematicBreak': return { type: 'thematicBreak', position: positionOf(node) };
    default: return null;
  }
}

function normalizeListItem(node: MdNode, definitions: ReadonlyMap<string, Definition>): NormalizedListItem | null {
  if (node.type !== 'listItem') return null;
  return { type: 'listItem', checked: node.checked ?? null, children: normalizeBlocks(node.children, definitions), position: positionOf(node) };
}

function normalizeBlocks(nodes: readonly MdNode[] | undefined, definitions: ReadonlyMap<string, Definition>): readonly NormalizedBlock[] {
  return (nodes ?? []).flatMap((node) => {
    const normalized = normalizeBlock(node, definitions);
    return normalized === null ? [] : [normalized];
  });
}

type Fence = Readonly<{ marker: string; size: number }>;

function openingFence(line: string): Fence | null {
  const match = /^ {0,3}(`{3,}|~{3,})/.exec(line);
  if (match === null) return null;
  const run = match[1] ?? '';
  const info = line.slice(match[0].length);
  if (run.startsWith('`') && info.includes('`')) return null;
  return { marker: run[0] ?? '`', size: run.length };
}

function closesFence(line: string, openFence: Fence): boolean {
  const match = /^ {0,3}(`{3,}|~{3,})/.exec(line);
  const run = match?.[1] ?? '';
  return run[0] === openFence.marker && run.length >= openFence.size
    && /^\s*$/.test(match === null ? '' : line.slice(match[0].length));
}

type HtmlBlock = Readonly<{ ends(line: string): boolean }>;

function htmlBlockStart(line: string): HtmlBlock | null {
  const start = /^ {0,3}/.exec(line)?.[0].length ?? 0;
  const content = line.slice(start);
  const rawTag = /^<(script|pre|style)(?:\s|>|$)/i.exec(content)?.[1];
  if (rawTag !== undefined) return { ends: (candidate) => new RegExp(`</${rawTag}\\s*>`, 'i').test(candidate) };
  if (content.startsWith('<!--')) return { ends: (candidate) => candidate.includes('-->') };
  if (content.startsWith('<?')) return { ends: (candidate) => candidate.includes('?>') };
  if (/^<!\[CDATA\[/i.test(content)) return { ends: (candidate) => candidate.includes(']]>') };
  if (/^<![A-Z]/.test(content)) return { ends: (candidate) => candidate.includes('>') };
  if (/^<\/?[A-Za-z]/.test(content)) {
    return { ends: (candidate) => /^\s*$/.test(candidate) };
  }
  return null;
}

function diagnosticsFor(markdown: string, root: MdNode): readonly MarkdownDiagnostic[] {
  const diagnostics: MarkdownDiagnostic[] = [];
  const lines = markdown.replace(/\r\n?/g, '\n').split('\n');
  let openFence: Readonly<{ marker: string; size: number; line: number }> | null = null;
  for (const [index, line] of lines.entries()) {
    if (openFence !== null) {
      if (closesFence(line, openFence)) openFence = null;
      continue;
    }
    const fence = openingFence(line);
    if (fence !== null) {
      openFence = { ...fence, line: index + 1 };
      continue;
    }
    const atx = /^ {0,3}(#{7,})(?:\s+|$)/.exec(line);
    if (atx !== null) diagnostics.push({ kind: 'malformed', message: 'ATX heading depth exceeds six', line: index + 1 });
    const quoteDepth = /^(?:>\s*)+/.exec(line)?.[0].split('>').length ?? 1;
    if (quoteDepth - 1 > 64) diagnostics.push({ kind: 'limit-exceeded', message: 'Block nesting exceeds 64 levels', line: index + 1 });
    const possibleDelimiter = lines[index + 1] ?? '';
    const tableShaped = line.includes('|') && possibleDelimiter.includes('|')
      && possibleDelimiter.includes('-') && /^[\s|:-]+$/.test(possibleDelimiter);
    if (tableShaped) {
      const headerCells = line.replace(/^\s*\||\|\s*$/g, '').split('|');
      const delimiterCells = possibleDelimiter.replace(/^\s*\||\|\s*$/g, '').split('|');
      const validDelimiter = delimiterCells.length === headerCells.length
        && delimiterCells.length > 0 && delimiterCells.every((cell) => /^\s*:?-+:?\s*$/.test(cell));
      if (!validDelimiter) diagnostics.push({ kind: 'malformed', message: 'Malformed GFM table delimiter', line: index + 2 });
    }
  }
  if (openFence !== null) diagnostics.push({ kind: 'malformed', message: 'Unclosed fenced code block', line: openFence.line });
  const visitHtml = (node: MdNode): void => {
    if (node.type === 'html') {
      diagnostics.push({
        kind: 'unsafe-raw-html', message: 'Raw HTML requires sanitization', line: node.position?.start.line ?? 1,
      });
    }
    for (const child of node.children ?? []) visitHtml(child);
  };
  visitHtml(root);
  return diagnostics;
}

const MAX_MARKDOWN_SOURCE_LENGTH = 2_000_000;
const MAX_MARKDOWN_NESTING_DEPTH = 64;

function inputLimitDiagnostic(markdown: string): MarkdownDiagnostic | null {
  if (markdown.length > MAX_MARKDOWN_SOURCE_LENGTH) {
    return { kind: 'limit-exceeded', message: 'Markdown source exceeds 2000000 characters', line: 1 };
  }
  const lines = markdown.replace(/\r\n?/g, '\n').split('\n');
  let openFence: Readonly<{ marker: string; size: number }> | null = null;
  let htmlBlock: HtmlBlock | null = null;
  for (const [index, line] of lines.entries()) {
    if (htmlBlock !== null) {
      if (htmlBlock.ends(line)) htmlBlock = null;
    } else {
      const startedHtmlBlock = htmlBlockStart(line);
      if (startedHtmlBlock !== null) {
        htmlBlock = startedHtmlBlock.ends(line) ? null : startedHtmlBlock;
      }
    }
    if (openFence !== null) {
      if (closesFence(line, openFence)) openFence = null;
      continue;
    }
    const fence = htmlBlock === null ? openingFence(line) : null;
    if (fence !== null) {
      openFence = fence;
      continue;
    }
    const indentation = /^( *)/.exec(line)?.[1].length ?? 0;
    const content = line.slice(indentation);
    const listMarker = /^(?:[-+*]|\d+[.)])\s/.test(content);
    const quoteDepth = (content.match(/^(?:>\s*)+/)?.[0].match(/>/g) ?? []).length;
    const listDepth = listMarker ? Math.floor(indentation / 2) + 1 : 0;
    if (quoteDepth + listDepth > MAX_MARKDOWN_NESTING_DEPTH) {
      return { kind: 'limit-exceeded', message: 'Block nesting exceeds 64 levels', line: index + 1 };
    }
  }
  return null;
}

export function parse(markdown: string): MarkdownParseResult {
  const limitDiagnostic = inputLimitDiagnostic(markdown);
  if (limitDiagnostic !== null) {
    return {
      status: 'failed',
      error: { kind: 'markdown-parse', message: 'Markdown input exceeds normalization limits', diagnostics: [limitDiagnostic] },
    };
  }
  try {
    const root = fromMarkdown(markdown, { extensions: [gfm()], mdastExtensions: [gfmFromMarkdown()] }) as MdNode;
    const definitions = new Map<string, Definition>();
    const collectDefinitions = (node: MdNode): void => {
      if (node.type === 'definition' && node.identifier !== undefined) {
        if (!definitions.has(node.identifier)) {
          definitions.set(node.identifier, { destination: node.url ?? '', title: node.title ?? null });
        }
      }
      for (const child of node.children ?? []) collectDefinitions(child);
    };
    collectDefinitions(root);
    const value: NormalizedMarkdownAst & { sourceLines?: readonly string[] } = {
      type: 'root', dialect: 'gfm', children: normalizeBlocks(root.children, definitions),
      diagnostics: diagnosticsFor(markdown, root), position: positionOf(root),
    };
    Object.defineProperty(value, 'sourceLines', {
      value: markdown.replace(/\r\n?/g, '\n').split('\n'), enumerable: false,
    });
    return {
      status: 'ready',
      value,
    };
  } catch (cause) {
    return {
      status: 'failed', error: { kind: 'markdown-parse', message: 'Markdown normalization failed', diagnostics: [], cause },
    };
  }
}

function inlineText(nodes: readonly NormalizedInline[], referenceText: 'visible' | 'source'): string {
  return nodes.map((node) => {
    if (node.type === 'text' || node.type === 'inlineCode') return node.value;
    if (node.type === 'image') return referenceText === 'source' && node.referenceSource !== undefined
      ? node.referenceSource : node.alt;
    if (node.type === 'break') return ' ';
    if (node.type === 'html') return '';
    if (node.type === 'link' && referenceText === 'source' && node.referenceSource !== undefined) {
      return node.referenceSource;
    }
    return inlineText(node.children, referenceText);
  }).join('').replace(/\s+/g, ' ').trim();
}

export function extractOutline<Context>(
  inputs: readonly OutlineInput<Context>[],
  options: ExtractOutlineOptions<Context>,
): HeadingOutline[] {
  const outline: HeadingOutline[] = [];
  let globalOrdinal = 0;
  for (const { ast, context } of inputs) {
    const sourceLines = (ast as NormalizedMarkdownAst & { sourceLines?: readonly string[] }).sourceLines;
    let localOrdinal = 0;
    const visit = (nodes: readonly NormalizedBlock[]): void => {
      for (const node of nodes) {
        if (options.traversal === 'recursive' && node.type === 'blockquote') visit(node.children);
        if (node.type === 'list') for (const item of node.children) visit(item.children);
      if (node.type !== 'heading' || node.depth > options.maxDepth) continue;
      if (options.traversal === 'line-level') {
        const sourceLine = sourceLines?.[node.position.start.line - 1];
        if (sourceLine !== undefined && /^[ \t]*#/.test(sourceLine) && !/^ {0,3}#/.test(sourceLine)) continue;
      }
      const text = inlineText(node.children, options.referenceText);
      if (options.textPolicy === 'non-empty-heading-label' && text.length === 0) continue;
      const input = { heading: node, globalOrdinal, localOrdinal, context };
      outline.push({
        depth: node.depth, id: options.headingId.createId(input), text, globalOrdinal, localOrdinal, position: node.position,
      });
      globalOrdinal += 1;
      localOrdinal += 1;
      }
    }
    visit(ast.children);
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
