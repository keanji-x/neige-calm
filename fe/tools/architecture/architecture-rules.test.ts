import { existsSync, readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { ESLint } from 'eslint';
import * as tsParser from '@typescript-eslint/parser';
import { describe, expect, it } from 'vitest';
import { createContextAllowlist, moduleRuntimeStateAllowlist } from './allowlists.mjs';
import { architecturePlugin } from './plugin.mjs';

const root = resolve(import.meta.dirname, '../..');
const fixtureRoot = resolve(import.meta.dirname, 'rule-fixtures');

async function lintFixture(ruleName: string, fixture: string) {
  const file = resolve(fixtureRoot, fixture);
  const eslint = new ESLint({
    overrideConfigFile: true,
    overrideConfig: [{
      files: ['**/*.{ts,tsx}'],
      languageOptions: { parser: tsParser, parserOptions: { project: false } },
      plugins: { architecture: architecturePlugin },
      rules: { [`architecture/${ruleName}`]: 'error' },
    }],
  });
  const [result] = await eslint.lintText(readFileSync(file, 'utf8'), { filePath: file });
  return result.messages.filter((message) => message.ruleId === `architecture/${ruleName}`);
}

describe('architecture/no-module-runtime-state', () => {
  const rejected = [
    ['top-level-let.ts', 'let current'],
    ['mutable-constructor.ts', 'new Map'],
    ['mutable-object.ts', 'cache = {}'],
    ['mutable-array.ts', 'entries = []'],
    ['static-mutable.ts', 'static current'],
    ['lazy-singleton.ts', '(() =>'],
    ['factory-call.ts', 'createStore()'],
  ] as const;
  for (const [fixture, entity] of rejected) {
    it(`rejects ${fixture}`, async () => {
      const messages = await lintFixture('no-module-runtime-state', fixture);
      expect(messages).toHaveLength(1);
      expect(messages.at(0)?.message).toContain(entity);
    });
  }

  const accepted = ['primitive.ts', 'function-declaration.ts', 'frozen-static-data.ts', 'declare-module.ts', 'schema.ts'] as const;
  for (const fixture of accepted) {
    it(`accepts ${fixture}`, async () => {
      expect(await lintFixture('no-module-runtime-state', fixture)).toHaveLength(0);
    });
  }
  it('ignores a shadowed imported binding', async () => {
    expect(await lintFixture('no-create-context-outside-allowlist', 'create-context-shadowed.ts')).toHaveLength(0);
  });
});

describe('architecture/no-create-context-outside-allowlist', () => {
  const rejected = [
    ['create-context-named.ts', 'createContext'],
    ['create-context-member.ts', 'React.createContext'],
    ['create-context-alias.ts', 'mk'],
  ] as const;
  for (const [fixture, entity] of rejected) {
    it(`rejects ${fixture}`, async () => {
      const messages = await lintFixture('no-create-context-outside-allowlist', fixture);
      expect(messages).toHaveLength(1);
      expect(messages.at(0)?.message).toContain(entity);
    });
  }
});

describe('architecture allowlists', () => {
  for (const [name, entries] of [
    ['module runtime state', moduleRuntimeStateAllowlist],
    ['createContext', createContextAllowlist],
  ] as const) {
    it(`${name} entries are explicit existing files`, () => {
      expect(entries.some((entry) => /[*?{}[\]]/.test(entry))).toBe(false);
      for (const entry of entries) expect(existsSync(resolve(root, entry)), entry).toBe(true);
    });
  }
});
