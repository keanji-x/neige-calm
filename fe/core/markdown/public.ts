export type MarkdownDepth = 1 | 2 | 3 | 4 | 5 | 6;

export type NormalizedText = Readonly<{ type: 'text'; value: string }>;
export type NormalizedInlineCode = Readonly<{ type: 'inline-code'; value: string }>;
export type NormalizedImage = Readonly<{ type: 'image'; alt: string; destination: string }>;
export type NormalizedDelete = Readonly<{ type: 'delete'; children: readonly NormalizedInline[] }>;
export type NormalizedInline = NormalizedText | NormalizedInlineCode | NormalizedImage | NormalizedDelete;

export type NormalizedHeading = Readonly<{
  type: 'heading';
  depth: MarkdownDepth;
  children: readonly NormalizedInline[];
}>;
export type NormalizedParagraph = Readonly<{
  type: 'paragraph';
  children: readonly NormalizedInline[];
}>;
export type NormalizedCode = Readonly<{
  type: 'code';
  language: string | null;
  value: string;
}>;
export type NormalizedTable = Readonly<{
  type: 'table';
  rows: readonly (readonly (readonly NormalizedInline[])[])[];
}>;
export type NormalizedThematicBreak = Readonly<{ type: 'thematic-break' }>;
export type NormalizedBlock =
  | NormalizedHeading
  | NormalizedParagraph
  | NormalizedCode
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

export type MarkdownParseFailure = Readonly<{
  kind: 'markdown-parse';
  message: string;
  cause?: unknown;
}>;

/** Parse failures are values, matching core/api and core/state; malformed source degrades with diagnostics. */
export type MarkdownParseResult =
  | Readonly<{ status: 'ready'; value: NormalizedMarkdownAst }>
  | Readonly<{ status: 'failed'; error: MarkdownParseFailure }>;

/** Heading labels have one closed rule: setext is included, fences excluded, image alt and inline code included. */
export type TextPolicy = 'heading-label';

export type HeadingIdInput<Context> = Readonly<{
  heading: NormalizedHeading;
  globalOrdinal: number;
  localOrdinal: number;
  context: Context;
}>;

/** Version 1 is ordinal-based and therefore independent of parser offsets and title text. */
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

export type ExtractOutlineOptions<Context> = Readonly<{
  maxDepth: MarkdownDepth;
  headingId: HeadingIdPolicy<Context>;
  textPolicy: TextPolicy;
  context: Context;
}>;

export type SanitizeAstPolicy = Readonly<{ rawHtml: 'drop' }>;
export type SafeMarkdownAst = NormalizedMarkdownAst;

function inlineNodes(source: string): readonly NormalizedInline[] {
  const nodes: NormalizedInline[] = [];
  let rest = source;
  while (rest.length > 0) {
    const image = rest.match(/!\[([^\]]*)\]\(([^)]*)\)/);
    const code = rest.match(/`([^`]*)`/);
    const deletion = rest.match(/~~([^~]+)~~/);
    const candidates = [image, code, deletion].filter((match): match is RegExpMatchArray => match !== null);
    let next: RegExpMatchArray | undefined;
    for (const candidate of candidates) {
      if (next === undefined || (candidate.index ?? 0) < (next.index ?? 0)) next = candidate;
    }
    if (next === undefined) {
      const plain = rest.replace(/\[([^\]]+)\]\([^)]*\)/g, '$1').replace(/[\\*_]/g, '');
      if (plain.length > 0) nodes.push({ type: 'text', value: plain });
      break;
    }
    const index = next.index ?? 0;
    if (index > 0) {
      const plain = rest.slice(0, index).replace(/\[([^\]]+)\]\([^)]*\)/g, '$1').replace(/[\\*_]/g, '');
      if (plain.length > 0) nodes.push({ type: 'text', value: plain });
    }
    if (next === image) nodes.push({ type: 'image', alt: next[1] ?? '', destination: next[2] ?? '' });
    else if (next === code) nodes.push({ type: 'inline-code', value: next[1] ?? '' });
    else nodes.push({ type: 'delete', children: inlineNodes(next[1] ?? '') });
    rest = rest.slice(index + next[0].length);
  }
  return nodes;
}

function isDepth(value: number): value is MarkdownDepth {
  return value >= 1 && value <= 6;
}

function tableDelimiter(line: string): boolean {
  const cells = line.replace(/^\||\|$/g, '').split('|');
  return cells.length > 0 && cells.every((cell) => /^\s*:?-{3,}:?\s*$/.test(cell));
}

function tableRow(line: string): readonly (readonly NormalizedInline[])[] {
  return line.replace(/^\||\|$/g, '').split('|').map((cell) => inlineNodes(cell.trim()));
}

export function parse(markdown: string): MarkdownParseResult {
  try {
    const lines = markdown.replace(/\r\n?/g, '\n').split('\n');
    const children: NormalizedBlock[] = [];
    const diagnostics: MarkdownDiagnostic[] = [];
    let lineIndex = 0;
    while (lineIndex < lines.length) {
      const line = lines[lineIndex] ?? '';
      if (line.trim().length === 0) {
        lineIndex += 1;
        continue;
      }
      const quoteDepth = line.match(/^(?:>\s*)+/)?.[0].match(/>/g)?.length ?? 0;
      if (quoteDepth > 64) diagnostics.push({ kind: 'limit-exceeded', message: 'Block nesting exceeds 64 levels', line: lineIndex + 1 });

      const fence = line.match(/^\s{0,3}(`{3,}|~{3,})(.*)$/);
      if (fence !== null) {
        const marker = fence[1] ?? '```';
        const language = (fence[2] ?? '').trim() || null;
        const body: string[] = [];
        let cursor = lineIndex + 1;
        while (cursor < lines.length && !(lines[cursor] ?? '').match(new RegExp(`^\\s{0,3}${marker[0]}{${marker.length},}\\s*$`))) {
          body.push(lines[cursor] ?? '');
          cursor += 1;
        }
        if (cursor >= lines.length) diagnostics.push({ kind: 'malformed', message: 'Unclosed fenced code block', line: lineIndex + 1 });
        children.push({ type: 'code', language, value: body.join('\n') });
        lineIndex = cursor < lines.length ? cursor + 1 : cursor;
        continue;
      }

      const atx = line.match(/^\s{0,3}(#{1,})(?:\s+|$)(.*)$/);
      if (atx !== null) {
        const depth = (atx[1] ?? '').length;
        if (isDepth(depth)) {
          const label = (atx[2] ?? '').replace(/\s+#+\s*$/, '').trim();
          children.push({ type: 'heading', depth, children: inlineNodes(label) });
        } else diagnostics.push({ kind: 'malformed', message: 'ATX heading depth exceeds six', line: lineIndex + 1 });
        lineIndex += 1;
        continue;
      }

      const nextLine = lines[lineIndex + 1] ?? '';
      const setext = nextLine.match(/^\s{0,3}(=+|-+)\s*$/);
      if (setext !== null) {
        children.push({ type: 'heading', depth: (setext[1] ?? '').startsWith('=') ? 1 : 2, children: inlineNodes(line.trim()) });
        lineIndex += 2;
        continue;
      }

      if (/^\s{0,3}((\*\s*){3,}|(-\s*){3,}|(_\s*){3,})$/.test(line)) {
        children.push({ type: 'thematic-break' });
        lineIndex += 1;
        continue;
      }

      if (line.includes('|') && nextLine.includes('|')) {
        if (tableDelimiter(nextLine)) {
          const rows: Array<readonly (readonly NormalizedInline[])[]> = [tableRow(line)];
          let cursor = lineIndex + 2;
          while (cursor < lines.length && (lines[cursor] ?? '').includes('|')) {
            rows.push(tableRow(lines[cursor] ?? ''));
            cursor += 1;
          }
          children.push({ type: 'table', rows });
          lineIndex = cursor;
          continue;
        }
        diagnostics.push({ kind: 'malformed', message: 'Malformed GFM table delimiter', line: lineIndex + 2 });
      }

      if (/<\/?(?:script|style|iframe|object|embed|img)\b/i.test(line)) {
        diagnostics.push({ kind: 'unsafe-raw-html', message: 'Raw HTML is retained only as literal text', line: lineIndex + 1 });
      }
      children.push({ type: 'paragraph', children: inlineNodes(line) });
      lineIndex += 1;
    }
    return { status: 'ready', value: { type: 'root', dialect: 'gfm', children, diagnostics } };
  } catch (cause) {
    return { status: 'failed', error: { kind: 'markdown-parse', message: 'Markdown normalization failed', cause } };
  }
}

function inlineText(nodes: readonly NormalizedInline[], policy: TextPolicy): string {
  if (policy !== 'heading-label') return '';
  return nodes.map((node) => {
    if (node.type === 'text' || node.type === 'inline-code') return node.value;
    if (node.type === 'image') return node.alt;
    return inlineText(node.children, policy);
  }).join('').replace(/\s+/g, ' ').trim();
}

export function extractOutline<Context>(
  ast: NormalizedMarkdownAst,
  options: ExtractOutlineOptions<Context>,
): HeadingOutline[] {
  const outline: HeadingOutline[] = [];
  let globalOrdinal = 0;
  let localOrdinal = 0;
  for (const node of ast.children) {
    if (node.type !== 'heading' || node.depth > options.maxDepth) continue;
    const input = { heading: node, globalOrdinal, localOrdinal, context: options.context };
    outline.push({
      depth: node.depth,
      id: options.headingId.createId(input),
      text: inlineText(node.children, options.textPolicy),
      globalOrdinal,
      localOrdinal,
    });
    globalOrdinal += 1;
    localOrdinal += 1;
  }
  return outline;
}

/** This is an AST boundary, not an HTML sanitizer or renderer allowlist. */
export function sanitizeAstPolicy(ast: NormalizedMarkdownAst, policy: SanitizeAstPolicy): SafeMarkdownAst {
  if (policy.rawHtml !== 'drop') return ast;
  return ast;
}
