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

function boundedOccurrenceOffsets(text: string, anchor: string): number[] {
  const offsets: number[] = [];
  let offset = text.indexOf(anchor);
  while (offset !== -1) {
    const before = text[offset - 1];
    const after = text[offset + anchor.length];
    if (!isIdentifierCharacter(before) && !isIdentifierCharacter(after)) offsets.push(offset);
    offset = text.indexOf(anchor, offset + 1);
  }
  return offsets;
}

function typescriptAnchorLines(path: string, contents: string, identifiers: readonly string[]): Map<string, Set<number>> {
  const result = new Map(identifiers.map((identifier) => [identifier, new Set<number>()]));
  const kind = path.endsWith('.tsx') ? ts.ScriptKind.TSX : path.endsWith('.jsx') ? ts.ScriptKind.JSX : ts.ScriptKind.TS;
  const sourceFile = ts.createSourceFile(path, contents, ts.ScriptTarget.Latest, false, kind);
  const code = Array.from({ length: contents.length }, () => ' ');
  const scanner = ts.createScanner(ts.ScriptTarget.Latest, true,
    kind === ts.ScriptKind.TSX || kind === ts.ScriptKind.JSX ? ts.LanguageVariant.JSX : ts.LanguageVariant.Standard,
    contents);
  for (let token = scanner.scan(); token !== ts.SyntaxKind.EndOfFileToken; token = scanner.scan()) {
    for (let offset = scanner.getTokenPos(); offset < scanner.getTextPos(); offset += 1) {
      code[offset] = contents[offset]!;
    }
  }
  const codeText = code.join('');
  for (const identifier of identifiers) {
    let offset = codeText.indexOf(identifier);
    while (offset !== -1) {
      const before = codeText[offset - 1];
      const after = codeText[offset + identifier.length];
      if (!isIdentifierCharacter(before) && !isIdentifierCharacter(after)) {
        result.get(identifier)!.add(sourceFile.getLineAndCharacterOfPosition(offset).line + 1);
      }
      offset = codeText.indexOf(identifier, offset + 1);
    }
  }
  return result;
}

function lineAtFieldOffset(startLine: number, field: string, offset: number): number {
  return startLine + (field.slice(0, offset).match(/\n/g)?.length ?? 0);
}

function postcssAnchorLines(contents: string, identifiers: readonly string[]): Map<string, Set<number>> {
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
          for (const offset of boundedOccurrenceOffsets(field.text, identifier)) {
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

export function codeAnchorLines(path: string, contents: string, identifiers: readonly string[]): Map<string, Set<number>> | null {
  const ext = extension(path);
  if (TYPESCRIPT_EXTENSIONS.has(ext)) return typescriptAnchorLines(path, contents, identifiers);
  if (ext === '.css') return postcssAnchorLines(contents, identifiers);
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

export function extractStatementIdentifiers(statement: unknown): string[] {
  if (typeof statement !== 'string') return [];
  const identifiers = new Set<string>();
  const addMatches = (text: string, pattern: RegExp): void => {
    for (const match of text.matchAll(pattern)) identifiers.add(match[0]);
  };
  for (const code of statement.matchAll(/`([^`]+)`/g)) {
    for (const match of code[1].matchAll(/\[[A-Za-z_][\w-]*(?:=[^\]]+)?\]|aria-[a-z0-9-]+|[.#][A-Za-z_][\w-]*|[A-Za-z_$][\w$-]*/g)) {
      const candidate = match[0];
      if (/^(?:aria-|[.#]|\[)/.test(candidate) || /[-_]/.test(candidate) || /[a-z][A-Z]|[A-Z].*[A-Z]/.test(candidate)) {
        identifiers.add(candidate);
      }
    }
  }
  addMatches(statement, /\[[A-Za-z_][\w-]*(?:=[^\]]+)?\]|aria-[a-z0-9-]+|[.#][A-Za-z_][\w-]*|[A-Za-z_$][\w$]*(?=[(])|[A-Za-z_$][\w$]*(?:_[\w$]+)+|[a-z]+[A-Z][\w$]*|[A-Z][a-z0-9]+(?:[A-Z][A-Za-z0-9$]*)+/g);
  return [...identifiers];
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
  const identifiers = extractStatementIdentifiers(statement).filter((identifier) => !ignoredIdentifiers.has(identifier));
  const locations = parseLocations(source);
  if (locations.some((location) => typeof location === 'string')) return empty;
  const sourceFiles = new Map<string, { contents: string; anchors: Map<string, Set<number>> | null }>();
  for (const location of locations) {
    if (typeof location !== 'string' && !sourceFiles.has(location.path)) {
      const contents = readFileSync(resolve(repoRoot, location.path), 'utf8');
      sourceFiles.set(location.path, { contents, anchors: codeAnchorLines(location.path, contents, identifiers) });
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

const ANCHOR_BASELINE_MAXIMUM = 218;
const ANCHOR_EXPIRY_CEILING = '2026-12-31';

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
  for (const raw of baselineRows) {
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) continue;
    const entry = raw as Record<string, unknown>;
    if (typeof entry.id === 'string' && (entry.subtype === 'not-in-file' || entry.subtype === 'range-miss')
      && typeof entry.reason === 'string' && entry.reason.trim() !== ''
      && typeof entry.expiry === 'string' && /^\d{4}-\d{2}-\d{2}$/.test(entry.expiry)
      && entry.expiry >= today && entry.expiry <= ANCHOR_EXPIRY_CEILING) {
      baseline.set(entry.id, entry.subtype);
    }
  }
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
  for (const [id, subtype] of actualBaseline) {
    if (baseline.get(id) !== subtype) add('<baseline>', id, 'source-anchor', `unbaselined ${subtype}`);
  }
  for (const [id, subtype] of baseline) {
    if (actualBaseline.get(id) !== subtype) add('<baseline>', id, 'source-anchor', `stale baseline ${subtype}`);
  }
  if (options.anchorBaselinePath
    && (baselineRows.length !== baseline.size || baselineRows.length !== actualBaseline.size)) {
    add('<baseline>', '<count>', 'source-anchor',
      `baseline count must equal actual count: declared ${baselineRows.length}, distinct valid ${baseline.size}, actual ${actualBaseline.size}`);
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
    anchorUnsupportedPath: resolve(repoRoot, 'docs/oracle/anchor-unsupported.yaml'),
  };
}
