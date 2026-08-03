import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { isAbsolute, relative, resolve, sep } from 'node:path';
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

function statementIdentifiers(statement: unknown): string[] {
  if (typeof statement !== 'string') return [];
  const identifiers = new Set<string>();
  const addMatches = (text: string, pattern: RegExp): void => {
    for (const match of text.matchAll(pattern)) identifiers.add(match[0]);
  };
  for (const code of statement.matchAll(/`([^`]+)`/g)) {
    addMatches(code[1], /aria-[a-z0-9-]+|[.#][A-Za-z_][\w-]*|[A-Za-z_$][\w$-]*/g);
  }
  addMatches(statement, /aria-[a-z0-9-]+|[.#][A-Za-z_][\w-]*|[A-Za-z_$][\w$]*(?=[(])|[A-Za-z_$][\w$]*(?:[_-][\w$-]+)+|[a-z]+[A-Z][\w$]*|[A-Z][a-z0-9]+(?:[A-Z][A-Za-z0-9$]*)+/g);
  return [...identifiers];
}

function sourceAnchorError(source: unknown, statement: unknown, repoRoot: string): string | null {
  if (typeof source !== 'string') return null;
  const identifiers = statementIdentifiers(statement);
  if (identifiers.length === 0) return null;
  const locations = parseLocations(source);
  if (locations.some((location) => typeof location === 'string')) return null;
  const citedText = locations.map((location) => {
    if (typeof location === 'string') return '';
    const lines = readFileSync(resolve(repoRoot, location.path), 'utf8').split(/\r?\n/);
    return lines.slice(location.start - 1, location.end).join('\n');
  }).join('\n');
  if (identifiers.some((identifier) => {
    const variants = identifier.startsWith('.') || identifier.startsWith('#')
      ? [identifier, identifier.slice(1)]
      : [identifier];
    return variants.some((variant) => citedText.includes(variant));
  })) return null;
  return `source ranges contain none of the statement identifiers: ${identifiers.join(', ')}`;
}

export function validateOracle(options: ValidateOptions): Violation[] {
  const owners = canonicalOwners(options.ownerAliasesPath);
  const files = readdirSync(options.oracleDir).filter((file) => file.endsWith('.yaml') && file !== 'owner-aliases.yaml').sort();
  const violations: Violation[] = [];
  const seen = new Map<string, string>();
  const retired = new Map<string, string>();
  const currentIds = new Set<string>();
  const add = (file: string, id: string, rule: string, message: string): void => { violations.push({ file, id, rule, message }); };

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
        const anchorError = sourceAnchorError(entry.source, entry.statement, options.repoRoot);
        if (anchorError) add(file, id, 'source-anchor', anchorError);
      }
      if (entry.authoritative_test !== 'NONE') {
        const testErrors = locationErrors(entry.authoritative_test, options.repoRoot);
        if (testErrors.length) add(file, id, 'authoritative-test-location', testErrors.join('; '));
      }
      if (typeof entry.why !== 'string' || entry.why.trim() === '') add(file, id, 'why-nonempty', 'why must be non-empty');
      if (typeof entry.statement !== 'string' || entry.statement.trim() === '') add(file, id, 'statement-nonempty', 'statement must be non-empty');
    });
  }
  return violations.sort((a, b) => `${a.id}\0${a.rule}\0${a.file}`.localeCompare(`${b.id}\0${b.rule}\0${b.file}`));
}

export function defaultOracleOptions(repoRoot: string): ValidateOptions {
  return {
    repoRoot,
    oracleDir: resolve(repoRoot, 'docs/oracle'),
    ownerAliasesPath: resolve(repoRoot, 'docs/oracle/owner-aliases.yaml'),
  };
}
