import { readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { parse } from 'yaml';
import { describe, expect, it } from 'vitest';

type ActionsDocument = {
  env?: Record<string, unknown>;
};

const actionsDirectory = resolve(import.meta.dirname, '../../../.github/workflows');

function disablesImplicitNpmAudit(document: ActionsDocument): boolean {
  return document.env?.NPM_CONFIG_AUDIT === 'false';
}

describe('GitHub Actions npm installs', () => {
  it('disables implicit npm audit at every workflow boundary', () => {
    const documents = readdirSync(actionsDirectory)
      .filter((name) => name.endsWith('.yml') || name.endsWith('.yaml'))
      .map((name) => ({
        name,
        document: parse(readFileSync(resolve(actionsDirectory, name), 'utf8')) as ActionsDocument,
      }));

    expect(documents.length).toBeGreaterThan(0);
    expect(documents.filter(({ document }) => !disablesImplicitNpmAudit(document))
      .map(({ name }) => name)).toEqual([]);
  });

  it.each([
    ['a missing setting', {}],
    ['an enabled setting', { env: { NPM_CONFIG_AUDIT: 'true' } }],
    ['an unquoted YAML boolean', parse('env:\n  NPM_CONFIG_AUDIT: false') as ActionsDocument],
  ])('rejects %s', (_case, document) => {
    expect(disablesImplicitNpmAudit(document)).toBe(false);
  });

  it('accepts the quoted false setting used by GitHub Actions', () => {
    const document = parse('env:\n  NPM_CONFIG_AUDIT: "false"') as ActionsDocument;
    expect(disablesImplicitNpmAudit(document)).toBe(true);
  });
});
