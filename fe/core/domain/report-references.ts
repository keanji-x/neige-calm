// Natural-language references. The Markdown parser and file admission keep their owners.
import { parse, sanitizeAstPolicy, type SafeBlock, type SafeInline } from '../markdown/public.js';
import { parseGitHubReferenceUrl, type GitHubReference } from './issue-url.js';
import { parseReportFileLink } from './report-file.js';

export type ReferenceRepository = Readonly<Pick<GitHubReference, 'owner' | 'name'>>;
export type ReportTextReference = Readonly<{ text: string; destination: string | null }>;

/** Only explicit links in visible prose establish a unique repository. Code/HTML are not context. */
export function reportReferenceRepository(markdown: readonly string[]): ReferenceRepository | null {
  let repository: ReferenceRepository | null = null;
  let ambiguous = false;
  const inlines = (nodes: readonly SafeInline[]) => {
    for (const node of nodes) {
      if (node.type === 'link') {
        const target = parseGitHubReferenceUrl(node.destination);
        if (target !== null) {
          if (repository !== null && (repository.owner !== target.owner || repository.name !== target.name)) ambiguous = true;
          else repository = { owner: target.owner, name: target.name };
        }
      }
      if (node.type === 'link' || node.type === 'strong' || node.type === 'emphasis' || node.type === 'delete') inlines(node.children);
    }
  };
  const blocks = (nodes: readonly SafeBlock[]) => {
    for (const node of nodes) {
      switch (node.type) {
        case 'heading': case 'paragraph': inlines(node.children); break;
        case 'blockquote': blocks(node.children); break;
        case 'list': for (const item of node.children) blocks(item.children); break;
        case 'table': for (const row of node.children) for (const cell of row.children) inlines(cell.children); break;
        case 'code': case 'thematicBreak': break;
      }
    }
  };
  for (const source of markdown) {
    const parsed = parse(source);
    if (parsed.status === 'ready') blocks(sanitizeAstPolicy(parsed.value, { rawHtml: 'drop' }).children);
  }
  return ambiguous ? null : repository;
}

/** This recognizes qualified paths, not ordinary identifiers, API routes or command fragments. */
export function qualifiedReportFileReference(value: string): boolean {
  return value.includes('/') && /^(?:\.{1,2}\/|\/)?[A-Za-z0-9_.-]+(?:\/[A-Za-z0-9_.-]+)*\.[A-Za-z][A-Za-z0-9]*(?::[0-9]+(?:-[0-9]+)?(?:,[0-9]+(?:-[0-9]+)?)*(?::[0-9]+)?)?$/.test(value)
    && parseReportFileLink(value) !== null;
}

/** Recognition preserves every character. Admission/navigation happens in the report renderer. */
export function reportTextReferences(text: string, repository: ReferenceRepository | null): readonly ReportTextReference[] {
  const pattern = /\b(?:issue|pr|pull request)\s*#[0-9]+\b|(?:\.{1,2}\/|\/)?[A-Za-z0-9_.-]+(?:\/[A-Za-z0-9_.-]+)*\.[A-Za-z][A-Za-z0-9]*(?::[0-9]+(?:-[0-9]+)?(?:,[0-9]+(?:-[0-9]+)?)*(?::[0-9]+)?)?/gi;
  const parts: ReportTextReference[] = [];
  let from = 0;
  for (const match of text.matchAll(pattern)) {
    const before = text[match.index - 1];
    // Never recognize an embedded path inside a URL, email, identifier or another path.
    if (before !== undefined && /[A-Za-z0-9_@/:.-]/.test(before)) continue;
    const spelling = match[0];
    const issue = /^(issue|pr|pull request)\s*#([0-9]+)$/i.exec(spelling);
    let destination: string | null = null;
    if (issue !== null && repository !== null) {
      const collection = issue[1].toLowerCase() === 'issue' ? 'issues' : 'pull';
      destination = parseGitHubReferenceUrl(`https://github.com/${repository.owner}/${repository.name}/${collection}/${issue[2]}`)?.url ?? null;
    } else if (qualifiedReportFileReference(spelling)) destination = spelling;
    if (destination === null) continue;
    if (match.index > from) parts.push({ text: text.slice(from, match.index), destination: null });
    parts.push({ text: spelling, destination });
    from = match.index + spelling.length;
  }
  if (from < text.length || parts.length === 0) parts.push({ text: text.slice(from), destination: null });
  return parts;
}
