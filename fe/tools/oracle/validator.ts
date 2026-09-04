import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { isAbsolute, relative, resolve, sep } from 'node:path';
import postcss, { type ChildNode } from 'postcss';
import ts from 'typescript';
import { parse } from 'yaml';

export interface Violation {
  id: string;
  rule: string;
  message: string;
  file: string;
}

export interface ValidateOptions {
  repoRoot: string;
  oracleDir: string;
  ownerAliasesPath: string;
  anchorNonePath?: string;
  anchorBaselinePath?: string;
  anchorPendingPath?: string;
  /** Overrides ANCHOR_PENDING_MAXIMUM; fixtures only, so the count cap is testable. */
  anchorPendingMaximum?: number;
  /** Overrides ANCHOR_PENDING_IDS; fixtures only, so the frozen-id-set rule is testable. */
  anchorPendingIds?: readonly string[];
  anchorUnsupportedPath?: string;
  today?: string;
}

export const ORACLE_RULES = Object.freeze([
  'document-shape', 'required-fields', 'enum-kind', 'enum-runtime_layer', 'enum-verification_owner', 'enum-test_tier',
  'enum-migration', 'id-format', 'id-kind-prefix', 'id-unique', 'former-id-format', 'former-id-unique', 'owner-slice', 'runtime-owner-layer',
  'skipped-fields', 'skipped-owner', 'non-skipped-reason', 'non-skipped-owner', 'intentional-omission-boolean',
  'source-location', 'source-anchor', 'authoritative-test-location', 'why-nonempty', 'statement-nonempty',
] as const);

export const ORACLE_YAML_FIELDS = Object.freeze([
  'id', 'former_id', 'kind', 'family', 'statement', 'why', 'source', 'authoritative_test', 'owner_slice',
  'intentional_omission', 'runtime_layer', 'verification_owner', 'test_tier', 'migration', 'skip_reason',
] as const);

const REQUIRED = ['id', 'kind', 'family', 'statement', 'why', 'source', 'authoritative_test', 'owner_slice',
  'intentional_omission', 'runtime_layer', 'verification_owner', 'test_tier', 'migration'] as const;
const ENUMS: Record<string, readonly unknown[]> = {
  kind: ['invariant', 'capability', 'gate'],
  runtime_layer: ['core', 'ui', 'systems', 'features', 'app', 'styles', 'none'],
  verification_owner: ['e2e', 'unit', 'lint', 'css', 'build', 'architecture', 'review-waiver', null],
  test_tier: ['browser', 'jsdom', 'static', 'none'],
  migration: ['pending', 'migrated', 'skipped'],
};
const KIND_PREFIX: Record<string, string> = { invariant: 'INV', capability: 'CAP', gate: 'GATE' };
const ID_PATTERN = /^(?:E2E-)?(INV|CAP|GATE)-(?:[A-Z0-9]+-)+\d{3}$/;
const LOCATION_PATTERN = /^([^\s:]+):(\d+)(?:-(\d+))?$/;

interface SourceLocation {
  path: string;
  start: number;
  end: number;
}

const TYPESCRIPT_EXTENSIONS = new Set(['.cjs', '.cts', '.js', '.jsx', '.mjs', '.mts', '.ts', '.tsx']);
const UNSUPPORTED_ANCHOR_EXTENSIONS = new Set([
  '.html', '.md', '.rs', '.sh', '.toml', '.txt', '.yaml', '.yml',
]);

function extension(path: string): string {
  const dot = path.lastIndexOf('.');
  return dot === -1 ? '' : path.slice(dot).toLowerCase();
}

function isIdentifierCharacter(character: string | undefined): boolean {
  return character !== undefined && /[A-Za-z0-9_$-]/.test(character);
}

function withoutCssComments(text: string): string {
  return text.replace(/\/\*[\s\S]*?\*\//g, (comment) => comment.replace(/[^\r\n]/g, ' '));
}

function boundedOccurrenceOffsets(text: string, anchor: string, caseInsensitive = false): number[] {
  const offsets: number[] = [];
  const haystack = caseInsensitive ? text.toLowerCase() : text;
  const needle = caseInsensitive ? anchor.toLowerCase() : anchor;
  let offset = haystack.indexOf(needle);
  while (offset !== -1) {
    const before = haystack[offset - 1];
    const after = haystack[offset + needle.length];
    if (!isIdentifierCharacter(before) && !isIdentifierCharacter(after)) offsets.push(offset);
    offset = haystack.indexOf(needle, offset + 1);
  }
  return offsets;
}

function typescriptAnchorLines(path: string, contents: string, identifiers: readonly string[],
  caseInsensitive: ReadonlySet<string>): Map<string, Set<number>> {
  const result = new Map(identifiers.map((identifier) => [identifier, new Set<number>()]));
  const kind = path.endsWith('.tsx') ? ts.ScriptKind.TSX : path.endsWith('.jsx') ? ts.ScriptKind.JSX : ts.ScriptKind.TS;
  // A standalone ts.createScanner cannot resume a template literal that contains `${}` — that needs the
  // parser's reScanTemplateToken — so it mis-tokenizes everything after the first such template and copies
  // the intervening comments into the buffer. Walk the parsed AST down to leaf tokens instead: token ranges
  // exclude trivia, so comments fall out while string/template/JSX text stays.
  const sourceFile = ts.createSourceFile(path, contents, ts.ScriptTarget.Latest, true, kind);
  const code = Array.from({ length: contents.length }, () => ' ');
  const retainLeafTokens = (node: ts.Node): void => {
    // getChildren surfaces attached JSDoc as children; JSDoc is trivia, so drop the whole subtree.
    if (node.kind >= ts.SyntaxKind.FirstJSDocNode && node.kind <= ts.SyntaxKind.LastJSDocNode) return;
    const children = node.getChildren(sourceFile);
    if (children.length > 0) {
      for (const child of children) retainLeafTokens(child);
      return;
    }
    for (let offset = node.getStart(sourceFile); offset < node.getEnd(); offset += 1) {
      code[offset] = contents[offset]!;
    }
  };
  retainLeafTokens(sourceFile);
  const codeText = code.join('');
  for (const identifier of identifiers) {
    for (const offset of boundedOccurrenceOffsets(codeText, identifier, caseInsensitive.has(identifier))) {
      result.get(identifier)!.add(sourceFile.getLineAndCharacterOfPosition(offset).line + 1);
    }
  }
  return result;
}

function lineAtFieldOffset(startLine: number, field: string, offset: number): number {
  return startLine + (field.slice(0, offset).match(/\n/g)?.length ?? 0);
}

function postcssAnchorLines(contents: string, identifiers: readonly string[],
  caseInsensitive: ReadonlySet<string>): Map<string, Set<number>> {
  const result = new Map(identifiers.map((identifier) => [identifier, new Set<number>()]));
  const root = postcss.parse(contents);
  const visit = (node: ChildNode): void => {
    if (node.type === 'comment') return;
    const startLine = node.source?.start?.line;
    if (startLine !== undefined) {
      const fields: Array<{ text: string; startLine: number }> = [];
      if (node.type === 'rule') fields.push({ text: withoutCssComments(node.selector), startLine });
      else if (node.type === 'decl') {
        const between = node.raws.between ?? '';
        fields.push({ text: node.prop, startLine });
        fields.push({
          text: withoutCssComments(node.value),
          startLine: lineAtFieldOffset(startLine, between, between.length),
        });
      } else if (node.type === 'atrule') {
        fields.push({ text: node.name, startLine });
        fields.push({
          text: withoutCssComments(node.params),
          startLine: lineAtFieldOffset(startLine, node.raws.afterName ?? '', (node.raws.afterName ?? '').length),
        });
      }
      for (const identifier of identifiers) {
        for (const field of fields) {
          for (const offset of boundedOccurrenceOffsets(field.text, identifier, caseInsensitive.has(identifier))) {
            result.get(identifier)!.add(lineAtFieldOffset(field.startLine, field.text, offset));
          }
        }
      }
    }
    if ('nodes' in node) node.nodes?.forEach(visit);
  };
  root.nodes.forEach(visit);
  return result;
}

/**
 * `caseInsensitive` names the subset of `identifiers` that match without regard to case. Only display-copy
 * anchors (§ extractStatementAnchors) belong in it: a statement paraphrases a UI section as `"waiting on
 * you"` while the source renders `Waiting on you`. Identifier-shaped anchors stay case-sensitive, because
 * `cardId` and `CardId` are different things in code and conflating them manufactures hits.
 */
export function codeAnchorLines(path: string, contents: string, identifiers: readonly string[],
  caseInsensitive: ReadonlySet<string> = new Set()): Map<string, Set<number>> | null {
  const ext = extension(path);
  if (TYPESCRIPT_EXTENSIONS.has(ext)) return typescriptAnchorLines(path, contents, identifiers, caseInsensitive);
  if (ext === '.css') return postcssAnchorLines(contents, identifiers, caseInsensitive);
  if (!UNSUPPORTED_ANCHOR_EXTENSIONS.has(ext)) {
    throw new Error(`source-anchor extension is not registered: ${ext || '<none>'} (${path})`);
  }
  return null;
}

function strings(value: unknown): string[] {
  if (typeof value === 'string') return [value];
  if (Array.isArray(value)) return value.flatMap(strings);
  if (value && typeof value === 'object') return Object.values(value).flatMap(strings);
  return [];
}

function canonicalOwners(path: string): Set<string> {
  const document: unknown = parse(readFileSync(path, 'utf8'));
  if (!document || typeof document !== 'object') return new Set();
  return new Set(strings(document).filter((value) => /^(?:core|ui|systems|features|app|styles|none)\//.test(value)));
}

function parseLocations(value: string): Array<SourceLocation | string> {
  const parsed: Array<SourceLocation | string> = [];
  for (const group of value.trim().split(/\s*;\s*|\s+\+\s+|\s+/)) {
    const [first, ...additionalRanges] = group.split(',');
    const firstMatch = LOCATION_PATTERN.exec(first);
    const locations = firstMatch
      ? [first, ...additionalRanges.map((range) => `${firstMatch[1]}:${range}`)]
      : [first, ...additionalRanges];
    for (const location of locations) {
      const match = LOCATION_PATTERN.exec(location);
      parsed.push(match
        ? { path: match[1], start: Number(match[2]), end: Number(match[3] ?? match[2]) }
        : `invalid location: ${location}`);
    }
  }
  return parsed;
}

function locationErrors(value: unknown, repoRoot: string): string[] {
  if (typeof value !== 'string') return ['must be a string location'];
  const errors: string[] = [];
  for (const location of parseLocations(value)) {
      if (typeof location === 'string') {
        errors.push(location);
        continue;
      }
      const { path, start, end } = location;
      const target = resolve(repoRoot, path);
      const relativeTarget = relative(repoRoot, target);
      if (isAbsolute(path) || relativeTarget === '..' || relativeTarget.startsWith(`..${sep}`) || isAbsolute(relativeTarget)) {
        errors.push(`path escapes repository: ${path}`);
        continue;
      }
      if (!existsSync(target)) {
        errors.push(`path does not exist: ${path}`);
        continue;
      }
      const contents = readFileSync(target, 'utf8');
      const lineCount = contents === '' ? 0 : contents.replace(/\r?\n$/, '').split(/\r?\n/).length;
      if (start < 1 || end < start || end > lineCount) errors.push(`line range ${start}-${end} outside ${path} (1-${lineCount})`);
  }
  return errors;
}

/**
 * Words that are NOT admissible anchors, however the author typed them.
 *
 * The bar an anchor has to clear is: "this token missing from the cited file is a real defect". A word that
 * occurs in almost every React/DOM source clears nothing — it turns `source:` into a formality, which is the
 * exact fake-green this rule exists to prevent. Each entry below is here because it is a language, framework
 * or CSS built-in whose presence in a cited file carries no information about the statement.
 *
 * Lower-cased on lookup; the list is deliberately short, because the shapes that admit a candidate at all
 * (backtick-quoted, ≥ 4 characters, path/dotted/word form) already exclude most prose.
 */
const GENERIC_ANCHOR_WORDS: ReadonlySet<string> = new Set([
  // React/DOM vocabulary present in essentially every component file.
  'children', 'props', 'state', 'ref', 'refs', 'key', 'keys', 'node', 'nodes', 'element', 'elements',
  'event', 'events', 'handler', 'handlers', 'render', 'component', 'components', 'style', 'styles',
  'class', 'classname', 'value', 'values', 'data', 'type', 'types', 'name', 'names', 'index', 'item',
  'items', 'list', 'lists', 'text', 'label', 'title', 'button', 'buttons', 'input',
  // `view` / `views`: a React/DOM word (`XtermView`, `view` state, `views` arrays) that also silently
  // exempts INV-UI-DIALOG-003, whose statement is a proof-of-absence ("the focus effect's deps must NOT
  // contain `view`"). Admitting `view` would red that entry for saying the word it forbids, which is the
  // inverse of what an anchor means. Disclosed here rather than left implicit, the way `change` is.
  'view', 'views',
  'form', 'span', 'null', 'true', 'false', 'undefined', 'void', 'this', 'that', 'return', 'async',
  'await', 'const', 'function', 'string', 'number', 'boolean', 'object', 'array', 'error', 'errors',
  // `change` / `changes` read as UI copy in a statement but land on a parameter name in the cited file —
  // the coincidental-hit shape this list exists to stop.
  'change', 'changes',
  // CSS built-ins: every stylesheet has them, so citing one proves nothing about the rule under test.
  'overflow', 'display', 'color', 'width', 'height', 'margin', 'padding', 'border', 'position',
  'absolute', 'relative', 'static', 'fixed', 'sticky', 'block', 'inline', 'flex', 'grid', 'none',
  'auto', 'hidden', 'visible', 'top', 'left', 'right', 'bottom', 'center', 'start', 'end',
  // Domain words so pervasive in this repo that every candidate file contains them.
  'card', 'cards', 'track', 'tracks', 'task', 'tasks', 'panel', 'panels', 'page', 'pages', 'user',
  'users', 'agent', 'agents', 'server', 'client', 'api', 'app', 'core', 'web', 'test', 'tests',
  // The same shape, found the hard way: each of these is a whole subsystem of this repo, so it occurs in
  // any file a statement about that subsystem could plausibly cite, and hitting it proves only that the
  // author cited a file from the right area — never that the cited *lines* carry the claim.
  //   `theme` / `themes`   — 89 sources: the theme token pipeline, every terminal theme, every fixture
  //                          that builds a track (`theme: { fg, bg }`). It is what made E2E-INV-INFRA-019
  //                          go green on a helper that sends a *valid* body, while that entry's statement
  //                          then claimed a body *missing* the field is rejected. (#1148 has since narrowed
  //                          the statement to the seed helper's own body literal, so it no longer claims the
  //                          rejection at all; the word stays on this list for the general reason above.)
  //   `terminal` / `terminals` — 110 sources: a card kind, a route segment, a CSS namespace.
  //   `codex`              — 105 sources: the agent backend's name, in imports, types and copy alike.
  //   `area` / `areas`     — 110/61 sources: the top-level container every track hangs off.
  //   `report` / `reports` — 117 sources: a card kind, a page, a rail, an API noun.
  'theme', 'themes', 'terminal', 'terminals', 'codex', 'area', 'areas', 'report', 'reports',
]);

/** An anchor plus how it must be matched; see `codeAnchorLines` for why only display copy ignores case. */
export interface StatementAnchor {
  text: string;
  /** Display copy is paraphrased in prose with free capitalisation, so it matches case-insensitively. */
  caseInsensitive: boolean;
}

const CJK_PATTERN = /[\u3000-\u303F\u3400-\u4DBF\u4E00-\u9FFF\uF900-\uFAFF\uFF00-\uFFEF]/;
/**
 * Runs that a display-copy anchor can never match, so the quote is cut at them and only the literal pieces
 * survive:
 *  - `<message>` / `{count}` — placeholders, replaced at run time; `Failed to load settings: <message>` is
 *    a template, and the only part that exists as a literal in the source is the prefix.
 *  - anything outside printable ASCII — `—`, `→`, typographic quotes. Sources routinely spell these as HTML
 *    entities (`&mdash;`) or escapes, so the character in the statement is not the bytes in the file.
 */
const UNMATCHABLE_RUN_PATTERN = /<[^<>]*>|\{[^{}]*\}|[^\x20-\x7E]+/g;
const DISPLAY_COPY_MINIMUM = 6;
const BACKTICK_WORD_MINIMUM = 4;
/** `children`, `overflow` — a bare word the author explicitly marked as code. */
const BACKTICK_WORD_PATTERN = /^[A-Za-z_$][A-Za-z0-9_$]*$/;
/** `/api/auth/whoami`, `web/src/ui`, `card.updated`, `snake_case_key` — path/dotted/underscored shapes. */
const BACKTICK_PATH_PATTERN = /^[/@]?[A-Za-z0-9_$][A-Za-z0-9_$./-]*$/;

function isGeneric(candidate: string): boolean {
  return GENERIC_ANCHOR_WORDS.has(candidate.toLowerCase());
}

/**
 * Display copy: text the statement quotes with `"…"` or `“…”`, i.e. words the author claims the UI shows.
 *
 * Restrictions, each of which exists because dropping it produced fake anchors on the real corpus:
 *  - Latin letters required, CJK rejected. Chinese inside quotes in these statements is prose emphasis
 *    (`不得因为"看起来无关"而重排`), not UI copy, and would never appear in a source file.
 *  - Unmatchable runs are cut out and every surviving literal piece becomes its own anchor. Taking only the
 *    longest piece is not enough: `"Issue dev / issue → PR autoflow"` is rendered as two sibling spans, and
 *    the longer half (`Issue dev / issue`) straddles the split while `PR autoflow` is right there in the
 *    JSX. Each surviving piece is still a ≥ 6-character quoted UI phrase, not a word that happens to recur.
 *  - A floor of DISPLAY_COPY_MINIMUM characters, so quoted noise (`"x"`, `"on"`) cannot become an anchor.
 */
function displayCopyAnchors(quoted: string): string[] {
  if (CJK_PATTERN.test(quoted) || !/[A-Za-z]/.test(quoted)) return [];
  return quoted.split(UNMATCHABLE_RUN_PATTERN)
    .map((piece) => piece.trim())
    .filter((piece) => piece.length >= DISPLAY_COPY_MINIMUM && /[A-Za-z]/.test(piece) && !isGeneric(piece));
}

export function extractStatementAnchors(statement: unknown): StatementAnchor[] {
  if (typeof statement !== 'string') return [];
  // Case-sensitive wins on collision: an anchor that is also identifier-shaped keeps the stricter match.
  const anchors = new Map<string, boolean>();
  const addIdentifier = (text: string): void => { anchors.set(text, false); };
  for (const code of statement.matchAll(/`([^`]+)`/g)) {
    for (const match of code[1].matchAll(/\[[A-Za-z_][\w-]*(?:=[^\]]+)?\]|aria-[a-z0-9-]+|[.#][A-Za-z_][\w-]*|[A-Za-z_$][\w$-]*/g)) {
      const candidate = match[0];
      if (/^(?:aria-|[.#]|\[)/.test(candidate) || /[-_]/.test(candidate) || /[a-z][A-Z]|[A-Z].*[A-Z]/.test(candidate)) {
        addIdentifier(candidate);
      }
    }
    // Whole-fragment shapes the shape-based scan above misses: `/dev/reset`, `web/src/ui`, `children`.
    const fragment = code[1].trim();
    const wordShaped = BACKTICK_WORD_PATTERN.test(fragment) && fragment.length >= BACKTICK_WORD_MINIMUM;
    const pathShaped = BACKTICK_PATH_PATTERN.test(fragment) && /[/._]/.test(fragment)
      && fragment.length >= BACKTICK_WORD_MINIMUM;
    if ((wordShaped || pathShaped) && !isGeneric(fragment)) addIdentifier(fragment);
  }
  for (const match of statement.matchAll(/\[[A-Za-z_][\w-]*(?:=[^\]]+)?\]|aria-[a-z0-9-]+|[.#][A-Za-z_][\w-]*|[A-Za-z_$][\w$]*(?=[(])|[A-Za-z_$][\w$]*(?:_[\w$]+)+|[a-z]+[A-Z][\w$]*|[A-Z][a-z0-9]+(?:[A-Z][A-Za-z0-9$]*)+/g)) {
    addIdentifier(match[0]);
  }
  for (const quoted of statement.matchAll(/"([^"\n]+)"|“([^”\n]+)”/g)) {
    for (const literal of displayCopyAnchors(quoted[1] ?? quoted[2] ?? '')) {
      if (!anchors.has(literal)) anchors.set(literal, true);
    }
  }
  return [...anchors].map(([text, caseInsensitive]) => ({ text, caseInsensitive }));
}

export function extractStatementIdentifiers(statement: unknown): string[] {
  return extractStatementAnchors(statement).map((anchor) => anchor.text);
}

type AnchorSubtype = 'not-in-file' | 'range-miss';

interface AnchorResult {
  error: string | null;
  subtype: AnchorSubtype | null;
  unsupported: string[];
}

function sourceAnchorResult(source: unknown, statement: unknown, repoRoot: string,
  ignoredIdentifiers: ReadonlySet<string>): AnchorResult {
  const empty: AnchorResult = { error: null, subtype: null, unsupported: [] };
  if (typeof source !== 'string') return empty;
  const anchors = extractStatementAnchors(statement).filter((anchor) => !ignoredIdentifiers.has(anchor.text));
  const identifiers = anchors.map((anchor) => anchor.text);
  const caseInsensitive = new Set(anchors.filter((anchor) => anchor.caseInsensitive).map((anchor) => anchor.text));
  const locations = parseLocations(source);
  if (locations.some((location) => typeof location === 'string')) return empty;
  const sourceFiles = new Map<string, { contents: string; anchors: Map<string, Set<number>> | null }>();
  for (const location of locations) {
    if (typeof location !== 'string' && !sourceFiles.has(location.path)) {
      const contents = readFileSync(resolve(repoRoot, location.path), 'utf8');
      sourceFiles.set(location.path, {
        contents, anchors: codeAnchorLines(location.path, contents, identifiers, caseInsensitive),
      });
    }
  }
  const supportedFiles = [...sourceFiles.values()].filter((file) => file.anchors !== null);
  const unsupported = locations.filter((location): location is SourceLocation =>
    typeof location !== 'string' && sourceFiles.get(location.path)?.anchors === null)
    .map((location) => `${location.path}:${location.start}${location.end === location.start ? '' : `-${location.end}`}`);
  if (identifiers.length === 0) return { error: null, subtype: null, unsupported };
  if (supportedFiles.length === 0) return { error: null, subtype: null, unsupported };
  const present = identifiers.filter((identifier) => supportedFiles.some((file) => file.anchors!.get(identifier)!.size > 0));
  if (present.length === 0) return {
    error: `statement identifiers do not occur in cited code files: ${identifiers.join(', ')}`,
    subtype: 'not-in-file', unsupported,
  };
  const anchored = locations.some((location) => {
    if (typeof location === 'string') return false;
    const lines = sourceFiles.get(location.path)?.anchors;
    return lines !== null && lines !== undefined && present.some((identifier) =>
      [...lines.get(identifier)!].some((line) => line >= location.start && line <= location.end));
  });
  return anchored ? { error: null, subtype: null, unsupported } : {
    error: `source ranges contain none of the statement identifiers: ${present.join(', ')}`,
    subtype: 'range-miss', unsupported,
  };
}

function parseStructuredList(path: string | undefined): unknown[] {
  if (!path || !existsSync(path)) return [];
  const value: unknown = parse(readFileSync(path, 'utf8'));
  return Array.isArray(value) ? value : [];
}

const ANCHOR_BASELINE_MAXIMUM = 173;
const ANCHOR_EXPIRY_CEILING = '2026-12-31';

// `anchor-pending.json` is NOT a second baseline. It holds the anchors the #1148 scanner fix exposed as
// never having anchored anything; each row is a defect awaiting a decision in #1170, and the list exists
// to be emptied — after which both the file and this code are deleted. Rules, each pinned by its own
// single-violation fixture:
//   1. exact match, no wildcards — an actual failure that is in neither account is `unbaselined`, and a row
//      whose entry no longer fails (or fails differently) is `stale pending`. Missing and extra both fail.
//   2. frozen id set — the admissible ids are enumerated below, in source. A row whose id is not in
//      ANCHOR_PENDING_IDS is rejected, so the list can only ever shrink: deleting a row is a data edit,
//      but admitting any id — including swapping one out for another at an unchanged row count — costs an
//      edit to this file. This is the load-bearing rule; ANCHOR_PENDING_MAXIMUM below is only a count cap
//      and on its own would let a fixed row be traded for a brand-new regression.
//   3. row shape — every row needs a subtype, an issue reference, and a note, each checked separately so
//      that dropping any one branch reds its own fixture.
//   4. no double accounting — an id present in both accounts is an error, never a silent exemption.
// The 38 ids the #1148 scanner fix exposed, less the 8 that #1148's anchor-strength pass turned real:
// stronger extraction gave those entries an anchor that actually holds, so their rows left the list.
// Rows may leave this list; nothing may enter it without a deliberate edit here, which is the visible
// act the mechanism exists to force.
const ANCHOR_PENDING_IDS: ReadonlySet<string> = new Set([
  'CAP-NEWTASK-029', 'E2E-CAP-ADDPANEL-005', 'E2E-CAP-ADDPANEL-007', 'E2E-CAP-CWD-005', 'E2E-CAP-DELETE-001',
  'E2E-CAP-RENAME-015', 'E2E-CAP-SYNC-009', 'E2E-CAP-TERMINAL-009', 'E2E-CAP-VIEWMODE-021',
  'E2E-CAP-TRACKCREATE-003', 'E2E-CAP-TRACKCREATE-007', 'E2E-CAP-TRACKCREATE-014', 'E2E-CAP-TRACKCREATE-020',
  'E2E-INV-ADDPANEL-009', 'E2E-INV-DELETE-002', 'E2E-INV-DELETE-005', 'E2E-INV-INFRA-038',
  'E2E-INV-LIFECYCLE-012', 'E2E-INV-REPORT-008', 'E2E-INV-SPECCHAT-011', 'E2E-INV-TERMINAL-005',
  'E2E-INV-TERMINAL-010', 'E2E-INV-TERMTHEME-003', 'E2E-INV-TERMTHEME-007', 'E2E-INV-TRACKCREATE-006',
  'E2E-INV-TRACKCREATE-011', 'E2E-INV-WHEEL-002', 'E2E-INV-WHEEL-003', 'INV-CARD-128', 'INV-SPECCONVO-004',
]);
const ANCHOR_PENDING_MAXIMUM = 30;
const PENDING_NOTE = 'anchor-pending.json is not a baseline: these anchors are known to anchor nothing, '
  + 'are tracked in #1170, and the list exists to be emptied';

export function validateOracle(options: ValidateOptions): Violation[] {
  const today = options.today ?? new Date().toISOString().slice(0, 10);
  const owners = canonicalOwners(options.ownerAliasesPath);
  const anchorNone = new Map<string, Set<string>>();
  for (const raw of parseStructuredList(options.anchorNonePath)) {
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) continue;
    const entry = raw as Record<string, unknown>;
    if (typeof entry.id === 'string' && Array.isArray(entry.identifiers)
      && entry.identifiers.every((identifier) => typeof identifier === 'string')) {
      anchorNone.set(entry.id, new Set(entry.identifiers));
    }
  }
  const baselineRows = parseStructuredList(options.anchorBaselinePath);
  const baseline = new Map<string, AnchorSubtype>();
  const baselineRowIds = new Set<string>();
  for (const raw of baselineRows) {
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) continue;
    const entry = raw as Record<string, unknown>;
    if (typeof entry.id === 'string') baselineRowIds.add(entry.id);
    if (typeof entry.id === 'string' && (entry.subtype === 'not-in-file' || entry.subtype === 'range-miss')
      && typeof entry.reason === 'string' && entry.reason.trim() !== ''
      && typeof entry.expiry === 'string' && /^\d{4}-\d{2}-\d{2}$/.test(entry.expiry)
      && entry.expiry >= today && entry.expiry <= ANCHOR_EXPIRY_CEILING) {
      baseline.set(entry.id, entry.subtype);
    }
  }
  const pendingRows = parseStructuredList(options.anchorPendingPath);
  const pending = new Map<string, AnchorSubtype>();
  const pendingRowIds = new Set<string>();
  const frozenPendingIds = new Set(options.anchorPendingIds ?? ANCHOR_PENDING_IDS);
  const pendingRowErrors: Array<[string, string]> = [];
  pendingRows.forEach((raw, index) => {
    const entry = raw && typeof raw === 'object' && !Array.isArray(raw) ? raw as Record<string, unknown> : {};
    // A row without a string id can never be in the frozen set, so it is rejected by that rule.
    const id = typeof entry.id === 'string' ? entry.id : `<row-${index + 1}>`;
    let admissible = true;
    if (!frozenPendingIds.has(id)) {
      pendingRowErrors.push([id, 'id is not in ANCHOR_PENDING_IDS, the frozen set in validator.ts: the list may'
        + ' only shrink, so admitting any id — including trading a fixed one for a new failure at the same row'
        + ' count — costs a deliberate source edit']);
      admissible = false;
    }
    if (pendingRowIds.has(id)) {
      pendingRowErrors.push([id, 'duplicate row: an id may appear at most once']);
      admissible = false;
    }
    pendingRowIds.add(id);
    if (entry.subtype !== 'not-in-file' && entry.subtype !== 'range-miss') {
      pendingRowErrors.push([id, `subtype must be "not-in-file" or "range-miss", got ${JSON.stringify(entry.subtype)}`]);
      admissible = false;
    }
    if (typeof entry.issue !== 'string' || !/^#\d+$/.test(entry.issue)) {
      pendingRowErrors.push([id, `issue must be a tracking reference like "#1170", got ${JSON.stringify(entry.issue)}`]);
      admissible = false;
    }
    if (typeof entry.note !== 'string' || entry.note.trim() === '') {
      pendingRowErrors.push([id, 'note must be a non-empty explanation of why the anchor never anchored']);
      admissible = false;
    }
    if (admissible) pending.set(id, entry.subtype as AnchorSubtype);
  });
  const unsupportedRows = parseStructuredList(options.anchorUnsupportedPath);
  const registeredUnsupported = new Map<string, string[]>();
  for (const raw of unsupportedRows) {
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) continue;
    const entry = raw as Record<string, unknown>;
    if (typeof entry.id === 'string' && Array.isArray(entry.locations)
      && entry.locations.every((location) => typeof location === 'string')
      && typeof entry.reason === 'string' && entry.reason.trim() !== ''
      && typeof entry.expiry === 'string' && /^\d{4}-\d{2}-\d{2}$/.test(entry.expiry)
      && entry.expiry >= today && entry.expiry <= ANCHOR_EXPIRY_CEILING) {
      registeredUnsupported.set(entry.id, entry.locations);
    }
  }
  const configFiles = new Set(['owner-aliases.yaml', 'anchor-none.yaml', 'anchor-unsupported.yaml']);
  const files = readdirSync(options.oracleDir).filter((file) => file.endsWith('.yaml') && !configFiles.has(file)).sort();
  const violations: Violation[] = [];
  const seen = new Map<string, string>();
  const retired = new Map<string, string>();
  const currentIds = new Set<string>();
  const actualBaseline = new Map<string, AnchorSubtype>();
  const actualUnsupported = new Map<string, string[]>();
  const add = (file: string, id: string, rule: string, message: string): void => { violations.push({ file, id, rule, message }); };
  if (baselineRows.length > ANCHOR_BASELINE_MAXIMUM) {
    add('<baseline>', '<count>', 'source-anchor',
      `baseline may only shrink: declared ${baselineRows.length}, maximum ${ANCHOR_BASELINE_MAXIMUM}`);
  }

  for (const file of files) {
    const value: unknown = parse(readFileSync(resolve(options.oracleDir, file), 'utf8'));
    if (!Array.isArray(value)) continue;
    for (const raw of value) {
      if (raw && typeof raw === 'object' && !Array.isArray(raw)) {
        const currentId = (raw as Record<string, unknown>).id;
        if (typeof currentId === 'string') currentIds.add(currentId);
      }
    }
  }

  for (const file of files) {
    const value: unknown = parse(readFileSync(resolve(options.oracleDir, file), 'utf8'));
    if (!Array.isArray(value)) {
      add(file, '<document>', 'document-shape', 'document must be a YAML sequence');
      continue;
    }
    value.forEach((raw, index) => {
      const entry = raw && typeof raw === 'object' && !Array.isArray(raw) ? raw as Record<string, unknown> : {};
      const id = typeof entry.id === 'string' ? entry.id : `<entry-${index + 1}>`;
      const missing = REQUIRED.filter((field) => !Object.hasOwn(entry, field));
      if (missing.length) add(file, id, 'required-fields', `missing: ${missing.join(', ')}`);
      if (Object.hasOwn(entry, 'family') && (typeof entry.family !== 'string' || entry.family.trim() === '')) {
        add(file, id, 'required-fields', 'family must be a non-empty string');
      }
      for (const [field, allowed] of Object.entries(ENUMS)) {
        if (Object.hasOwn(entry, field) && !allowed.includes(entry[field])) add(file, id, `enum-${field}`, `invalid ${field}`);
      }
      const idMatch = typeof entry.id === 'string' ? ID_PATTERN.exec(entry.id) : null;
      if (!idMatch) add(file, id, 'id-format', 'id must contain KIND, uppercase domain, and a three-digit sequence');
      else if (ENUMS.kind.includes(entry.kind) && KIND_PREFIX[String(entry.kind)] !== idMatch[1]) add(file, id, 'id-kind-prefix', 'id KIND does not match kind');
      if (typeof entry.id === 'string') {
        const previous = seen.get(entry.id);
        if (previous) add(file, id, 'id-unique', `duplicate also found in ${previous}`);
        else seen.set(entry.id, file);
      }
      if (Object.hasOwn(entry, 'former_id')) {
        if (typeof entry.former_id !== 'string' || !ID_PATTERN.test(entry.former_id) || entry.former_id === entry.id) {
          add(file, id, 'former-id-format', 'former_id must be a different, valid retired id');
        } else {
          const previous = retired.get(entry.former_id);
          if (currentIds.has(entry.former_id) || previous) {
            add(file, id, 'former-id-unique', previous
              ? `former_id duplicate also found in ${previous}`
              : 'former_id collides with a current id');
          } else retired.set(entry.former_id, file);
        }
      }
      if (typeof entry.owner_slice !== 'string' || !owners.has(entry.owner_slice)) add(file, id, 'owner-slice', 'owner_slice is not canonical');
      if (typeof entry.owner_slice === 'string' && ENUMS.runtime_layer.includes(entry.runtime_layer) && entry.runtime_layer !== entry.owner_slice.split('/')[0]) add(file, id, 'runtime-owner-layer', 'runtime_layer differs from owner_slice prefix');
      if (entry.migration === 'skipped') {
        if (typeof entry.skip_reason !== 'string' || entry.skip_reason.trim() === '') add(file, id, 'skipped-fields', 'skipped entry requires skip_reason');
        if (entry.verification_owner !== null) add(file, id, 'skipped-owner', 'skipped entry requires null verification_owner');
      } else {
        if (Object.hasOwn(entry, 'skip_reason')) add(file, id, 'non-skipped-reason', 'non-skipped entry must not have skip_reason');
        if (entry.verification_owner === null) add(file, id, 'non-skipped-owner', 'non-skipped entry requires verification_owner');
      }
      if (typeof entry.intentional_omission !== 'boolean') add(file, id, 'intentional-omission-boolean', 'intentional_omission must be boolean');
      const sourceErrors = locationErrors(entry.source, options.repoRoot);
      if (sourceErrors.length) add(file, id, 'source-location', sourceErrors.join('; '));
      else {
        const anchor = sourceAnchorResult(entry.source, entry.statement, options.repoRoot, anchorNone.get(id) ?? new Set());
        if (anchor.unsupported.length) actualUnsupported.set(id, anchor.unsupported);
        if (anchor.error && anchor.subtype) actualBaseline.set(id, anchor.subtype);
      }
      if (entry.authoritative_test !== 'NONE') {
        const testErrors = locationErrors(entry.authoritative_test, options.repoRoot);
        if (testErrors.length) add(file, id, 'authoritative-test-location', testErrors.join('; '));
      }
      if (typeof entry.why !== 'string' || entry.why.trim() === '') add(file, id, 'why-nonempty', 'why must be non-empty');
      if (typeof entry.statement !== 'string' || entry.statement.trim() === '') add(file, id, 'statement-nonempty', 'statement must be non-empty');
    });
  }
  // An actual failure is accounted for by exactly one of the two lists; an id claimed by both is an error
  // (rule 3) and is attributed to the baseline, so no count check double-reports the same overlap.
  const heldByPending = new Set([...actualBaseline]
    .filter(([id, subtype]) => baseline.get(id) !== subtype && pending.get(id) === subtype)
    .map(([id]) => id));
  for (const [id, subtype] of actualBaseline) {
    if (baseline.get(id) !== subtype && !heldByPending.has(id)) {
      add('<baseline>', id, 'source-anchor', `unbaselined ${subtype}`);
    }
  }
  for (const [id, subtype] of baseline) {
    if (actualBaseline.get(id) !== subtype) add('<baseline>', id, 'source-anchor', `stale baseline ${subtype}`);
  }
  const accountedByBaseline = actualBaseline.size - heldByPending.size;
  if (options.anchorBaselinePath
    && (baselineRows.length !== baseline.size || baselineRows.length !== accountedByBaseline)) {
    add('<baseline>', '<count>', 'source-anchor',
      `baseline count must equal actual count: declared ${baselineRows.length}, distinct valid ${baseline.size}, actual ${accountedByBaseline}`);
  }
  if (options.anchorPendingPath) {
    const maximum = options.anchorPendingMaximum ?? ANCHOR_PENDING_MAXIMUM;
    if (pendingRows.length > maximum) {
      add('<pending>', '<count>', 'source-anchor',
        `pending list may only shrink: declared ${pendingRows.length}, maximum ${maximum} — ${PENDING_NOTE}`);
    }
    for (const [id, message] of pendingRowErrors) {
      add('<pending>', id, 'source-anchor', `${message} — ${PENDING_NOTE}`);
    }
    for (const [id, subtype] of pending) {
      if (actualBaseline.get(id) !== subtype) {
        add('<pending>', id, 'source-anchor',
          `stale pending ${subtype}: this entry no longer fails that way, so delete its row — ${PENDING_NOTE}`);
      }
    }
    for (const id of pendingRowIds) {
      if (baselineRowIds.has(id)) {
        add('<pending>', id, 'source-anchor',
          `id is in both anchor-baseline.json and anchor-pending.json; a debt belongs to exactly one account — ${PENDING_NOTE}`);
      }
    }
  }
  if (options.anchorUnsupportedPath) {
    if (unsupportedRows.length !== registeredUnsupported.size) {
      add('<unsupported>', '<count>', 'source-anchor',
        `unsupported count must equal distinct valid count: declared ${unsupportedRows.length}, distinct valid ${registeredUnsupported.size}`);
    }
    const unsupportedIds = new Set([...actualUnsupported.keys(), ...registeredUnsupported.keys()]);
    for (const id of unsupportedIds) {
      const actual = actualUnsupported.get(id) ?? [];
      const registered = registeredUnsupported.get(id) ?? [];
      if (JSON.stringify(actual) !== JSON.stringify(registered)) {
        add('<unsupported>', id, 'source-anchor', `unsupported locations changed: expected [${registered.join(', ')}], actual [${actual.join(', ')}]`);
      }
    }
  }
  return violations.sort((a, b) => `${a.id}\0${a.rule}\0${a.file}`.localeCompare(`${b.id}\0${b.rule}\0${b.file}`));
}

export function defaultOracleOptions(repoRoot: string): ValidateOptions {
  return {
    repoRoot,
    oracleDir: resolve(repoRoot, 'docs/oracle'),
    ownerAliasesPath: resolve(repoRoot, 'docs/oracle/owner-aliases.yaml'),
    anchorNonePath: resolve(repoRoot, 'docs/oracle/anchor-none.yaml'),
    anchorBaselinePath: resolve(repoRoot, 'fe/tools/oracle/anchor-baseline.json'),
    anchorPendingPath: resolve(repoRoot, 'fe/tools/oracle/anchor-pending.json'),
    anchorUnsupportedPath: resolve(repoRoot, 'docs/oracle/anchor-unsupported.yaml'),
  };
}
